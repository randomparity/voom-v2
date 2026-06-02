use std::collections::{HashMap, VecDeque};

use crate::OperationResponse;

// Bounds the idempotency cache. Because in-flight (Active) entries are never
// evicted and `begin` refuses new work once the cache is full of them (503
// ServiceAtCapacity), this also caps concurrent in-flight operations per
// worker. Keep it comfortably above the expected concurrent lease count.
pub(super) const IDEMPOTENCY_CACHE_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub(crate) struct CachedResponse {
    pub(crate) response: OperationResponse,
    pub(crate) body: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum IdempotencyBegin {
    Replay(CachedResponse),
    Duplicate {
        key: String,
        original_status: String,
    },
    Started,
    AtCapacity,
}

#[derive(Debug)]
enum IdempotencyStatus {
    Active {
        hash: [u8; 32],
    },
    Completed {
        hash: [u8; 32],
        response: CachedResponse,
    },
}

#[derive(Debug)]
pub(crate) struct CacheEntry {
    status: IdempotencyStatus,
}

#[derive(Debug)]
pub(crate) struct IdempotencyCache {
    capacity: usize,
    order: VecDeque<String>,
    pub(crate) entries: HashMap<String, CacheEntry>,
}

impl IdempotencyCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    pub(crate) fn lookup(&self, key: &str, hash: [u8; 32]) -> IdempotencyBegin {
        let Some(entry) = self.entries.get(key) else {
            return IdempotencyBegin::Started;
        };
        match &entry.status {
            IdempotencyStatus::Active { .. } => IdempotencyBegin::Duplicate {
                key: key.to_owned(),
                original_status: "active".to_owned(),
            },
            IdempotencyStatus::Completed {
                hash: existing_hash,
                response,
            } if *existing_hash == hash => IdempotencyBegin::Replay(response.clone()),
            IdempotencyStatus::Completed { .. } => IdempotencyBegin::Duplicate {
                key: key.to_owned(),
                original_status: "completed".to_owned(),
            },
        }
    }

    pub(crate) fn begin(&mut self, key: String, hash: [u8; 32]) -> IdempotencyBegin {
        match self.lookup(&key, hash) {
            IdempotencyBegin::Started => {}
            other => return other,
        }
        if self.capacity == 0 {
            return IdempotencyBegin::Started;
        }
        if !self.make_room() {
            // Cache is full of in-flight entries and nothing is evictable.
            // Refuse the new operation rather than admit it untracked: an
            // untracked key loses in-flight duplicate rejection, which would
            // let a concurrent duplicate execute twice. Returning capacity
            // backpressure keeps memory bounded and the dedup guarantee intact.
            return IdempotencyBegin::AtCapacity;
        }
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CacheEntry {
                status: IdempotencyStatus::Active { hash },
            },
        );
        IdempotencyBegin::Started
    }

    pub(crate) fn complete(&mut self, key: &str, hash: [u8; 32], response: CachedResponse) {
        if self.capacity == 0 {
            return;
        }
        if let Some(entry) = self.entries.get_mut(key) {
            match &entry.status {
                IdempotencyStatus::Active {
                    hash: existing_hash,
                }
                | IdempotencyStatus::Completed {
                    hash: existing_hash,
                    ..
                } if *existing_hash == hash => {
                    entry.status = IdempotencyStatus::Completed { hash, response };
                }
                _ => {}
            }
            return;
        }
        if !self.make_room() {
            return;
        }
        let key = key.to_owned();
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CacheEntry {
                status: IdempotencyStatus::Completed { hash, response },
            },
        );
    }

    pub(crate) fn clear_active(&mut self, key: &str, hash: [u8; 32]) {
        let should_remove = self.entries.get(key).is_some_and(|entry| {
            matches!(
                entry.status,
                IdempotencyStatus::Active {
                    hash: active_hash
                } if active_hash == hash
            )
        });
        if should_remove {
            self.entries.remove(key);
            self.order.retain(|queued| queued != key);
        }
    }

    /// Evicts completed entries oldest-first until the cache is below capacity.
    ///
    /// Active (in-flight) entries are never evicted; they are skipped so a
    /// completed entry behind them can still be reclaimed. Returns `true` when
    /// there is room for a new entry, and `false` when the cache is full of
    /// in-flight entries and nothing could be evicted — callers must not insert
    /// in that case, or `IDEMPOTENCY_CACHE_CAPACITY` is defeated.
    fn make_room(&mut self) -> bool {
        let mut active_seen = 0;
        while self.entries.len() >= self.capacity && active_seen < self.order.len() {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            match self.entries.get(&oldest) {
                Some(entry) if matches!(entry.status, IdempotencyStatus::Completed { .. }) => {
                    self.entries.remove(&oldest);
                }
                Some(_) => {
                    self.order.push_back(oldest);
                    active_seen += 1;
                }
                None => {}
            }
        }
        self.entries.len() < self.capacity
    }
}
