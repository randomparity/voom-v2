//! Bench-only runner shared by the four `observe-*` examples (#838), so every measured path
//! is timed by the same code. See `scripts/media-evidence-observe.py` for the harness.
//!
//! Reads paths from standard input until an empty line, observes them with `<concurrency>`
//! tasks (the one argument), and writes one line per file:
//! `ok <index> <start> <seconds> <size> <content_hash>` or
//! `err <index> <start> <seconds> <variant>`. Errors carry the variant name only, because
//! their messages can hold a path. It then writes `done` and waits for end of input, so the
//! harness can read this process's counters before it exits.

use std::collections::VecDeque;
use std::fmt::Arguments;
use std::future::Future;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

/// Size and content hash of an observed file, or the error variant name.
pub type Observed = Result<(u64, String), &'static str>;

pub async fn run<F, Fut>(observe: F) -> io::Result<()>
where
    F: Fn(PathBuf) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Observed> + Send,
{
    let concurrency = concurrency()?;
    let mut paths = VecDeque::new();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.is_empty() {
            break;
        }
        paths.push_back((paths.len(), PathBuf::from(line)));
    }
    let queue = Arc::new(Mutex::new(paths));
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..concurrency {
        let queue = Arc::clone(&queue);
        let observe = observe.clone();
        tasks.spawn(async move {
            loop {
                let next = queue
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .pop_front();
                let Some((index, path)) = next else {
                    return Ok::<(), io::Error>(());
                };
                let start = started.elapsed().as_secs_f64();
                let result = observe(path).await;
                let seconds = started.elapsed().as_secs_f64() - start;
                match result {
                    Ok((size, hash)) => emit(format_args!(
                        "ok {index} {start:.6} {seconds:.6} {size} {hash}"
                    ))?,
                    Err(variant) => {
                        emit(format_args!(
                            "err {index} {start:.6} {seconds:.6} {variant}"
                        ))?;
                    }
                }
            }
        });
    }
    while let Some(joined) = tasks.join_next().await {
        joined.map_err(io::Error::other)??;
    }
    emit(format_args!("done"))?;
    for line in io::stdin().lock().lines() {
        line?;
    }
    Ok(())
}

fn concurrency() -> io::Result<usize> {
    let invalid = || io::Error::other("usage: observe-<path> <concurrency of 1 or more>");
    let value: usize = std::env::args()
        .nth(1)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if value == 0 {
        return Err(invalid());
    }
    Ok(value)
}

fn emit(line: Arguments<'_>) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{line}")?;
    out.flush()
}
