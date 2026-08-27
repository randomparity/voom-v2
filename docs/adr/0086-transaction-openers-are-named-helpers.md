# 0086 — Transaction openers are named helpers

## Status

Accepted (2026-08-26)

## Context

[ADR 0083](0083-read-then-write-transactions-begin-immediate.md) states the rule:
a transaction whose **first** statement reads and whose **later** statements write
must open with `BEGIN IMMEDIATE`. `SQLite` refuses the read→write lock upgrade
with `SQLITE_BUSY` *without* invoking the busy handler, so the pool's 30s
`busy_timeout` is never consulted and the caller fails instead of waiting.
Issue #546 is the most recent flake traced to that shape; ADR 0083 lists four
call sites converted one at a time as earlier flakes surfaced, and defers the
rest.

Nothing enforces the rule. Issue #552 asks for a guardrail.

**Where transactions are opened.** A one-time interprocedural analysis of the two
production crates that touch `sqlx` found **186 pool-level transactions**:

| crate | transactions | opened by |
|---|---:|---|
| `voom-control-plane` | 106 | 105 through `begin_tx` / `begin_immediate_tx`; 1 direct |
| `voom-store` | 86 | 60 direct `pool.begin*()`; 26 through 7 ad-hoc local helpers |

Control-plane already has the pattern this record generalises, and is 99%
consistent with it. It does not construct SQL — `check-control-plane-sql-boundary`
enforces that — but it does own the *transaction boundary*, because a use case
composes several repository calls that must commit atomically:

```rust
let mut tx = begin_tx(&self.pool).await?;
let job = self.jobs.succeed_in_tx(&mut tx, id, now).await?;  // one repo
append_event(&self.events, &mut tx, …).await?;               // another
commit_tx(tx).await?;
```

`voom-store` is the crate ADR 0083 described as having "three spellings of this
transaction". It has seven: `begin` appears three times in different modules,
alongside `begin_immediate`, `begin_tx`, and `begin_gate_tx`, plus 60 raw
`pool.begin()` calls.

**What the openers do not say.** `pool.begin()` and `begin_tx` record a
mechanism, not an intent. Reading either tells you the mode was deferred; it does
not tell you whether the author considered what the transaction's first statement
does. Of the 186, **24 are confirmed read-then-write on a deferred opener** —
live instances of #546's defect class — and none of their call sites reads
differently from the 67 deferred openers that are genuinely safe.

**Why a static analyzer was tried first, and abandoned.** This record originally
specified an interprocedural analyzer recovering read/write ordering from source.
It was built and it worked: 186 transactions classified, 24 violations found. It
also reached ~900 lines of Python, and every correction that made it more honest
made its output worse — closing one fail-open path moved 22 transactions from
"classified" to "unclassifiable", against a 30-entry disposition budget. The
analysis was recovering, through 556 `&mut Transaction` signatures, a fact the
author held at the keystroke and discarded.

## Decision

**Every pool-level transaction is opened by a helper whose name states the shape
of the transaction.** `voom-store` gains one module with three functions, `pub`
so `voom-control-plane` uses the same vocabulary:

| helper | mode | for |
|---|---|---|
| `begin_read_then_write` | `BEGIN IMMEDIATE` | reads before it writes — ADR 0083's hazard |
| `begin_write_first` | `BEGIN` | first statement writes, so the lock is taken up front |
| `begin_read_only` | `BEGIN` | never writes, so it never upgrades |

The guardrail is then a **boundary check**: no production code calls
`.begin()` or `.begin_with()` outside that module. That is a single `ast-grep`
rule, and it replaces the analyzer entirely.

Five sub-decisions follow.

**The name is the classification.** `begin_write_first` and `begin_read_only`
compile to the same `BEGIN`, and are not merged. Two names for one mechanism is
usually a defect; here the difference is the record. A reviewer reading a diff
sees a claim about the transaction's shape and can check it against the body,
which is not true of `begin_tx`.

**The check proves deliberateness, not correctness.** A caller can still pick
`begin_write_first` for a body that reads first. That is a weaker guarantee than
the analyzer attempted, and it is accepted: the failure mode becomes a visible
wrong claim at one call site, reviewable in a diff, rather than an invisible
omission recoverable only by whole-program analysis.

**The analyzer becomes the migration tool.** Its classification of all 186
transactions is the input to the conversion, and it is not kept afterwards. A
one-time correct answer is worth more than a permanently maintained approximate
one.

**`voom-store`'s seven ad-hoc helpers are removed, not left beside the new
module.** ADR 0083 rejected adding a shared helper because "a fourth entry point
would have to replace the others to be worth adding, which is a refactor of its
own and not this defect." That is a scoping judgment about fixing one flake, and
it names the condition under which the helper is worth adding. This change meets
it.

**Savepoints are out of scope.** `tx.begin()` on a live handle opens a savepoint,
which cannot upgrade a pool lock. The boundary check matches the pool receiver,
not the method name.

## Consequences

Every transaction in the workspace carries a stated shape, and a reader of any
call site learns the author's intent without reading the body.

The migration touches ~186 call sites across two crates. It is mechanical and
census-driven, but it is not small, and it is the bulk of the change #552 ships.

The guardrail's permanent surface is one `ast-grep` rule and its selftest —
roughly 20 lines against the analyzer's 900, with no allow file and no
unclassifiable residue to disposition.

The check cannot catch a mislabelled opener. A future record may add a narrower
verification on top of the vocabulary — with every opener a known helper call,
the analysis no longer needs to discover openers or resolve factories, which were
the two hardest parts of the abandoned one.

`voom-control-plane`'s `begin_tx` and `begin_immediate_tx` are deleted; its 105
call sites move to the shared vocabulary. `voom-store`'s helper module is `pub`,
which widens that crate's API by three functions.

## Considered & rejected

- **A static analyzer that verifies the ordering rule.** The original decision
  here. verified: built to 186 transactions and 24 violations at ~900 lines of
  Python (branch `feat/enforce-begin-immediate-552`, abandoned); making its call
  path fail closed moved unclassifiable from 7 to 29 against a 30-entry budget.
  judgment: it recovers interprocedurally what the author knew at the call site,
  and its accuracy degrades as its honesty improves.
- **Type-state — `Transaction<Deferred, HasRead>`, with `write()` unavailable on
  a deferred handle that has read.** The structurally correct answer: it encodes
  the rule exactly, at compile time, with no runtime cost. verified: the codebase
  has 556 functions taking `&mut Transaction` and 382 statements executing
  against `&mut **tx`; a state parameter that changes as statements execute
  cannot be threaded through `&mut`, because the type behind a mutable reference
  cannot change. judgment: it would require converting the whole persistence
  layer to ownership-passing (`let (tx, v) = self.inner(tx).await?;`) through
  call chains three and four deep — a rewrite of the calling convention, not a
  refactor.
- **Mode-only type-state — `&mut Tx<Immediate>`, with writes requiring the
  immediate type.** Works with `&mut`, because the type never changes.
  verified: it can only express "any transaction that writes needs `IMMEDIATE`",
  which the census prices at 145 transactions forced to `IMMEDIATE` to catch 59
  — 86 needless lock holds, including the `UPDATE … WHERE id IN (SELECT …)`
  shape ADR 0083 names as already honouring `busy_timeout`.
- **Convert every deferred `pool.begin()` to `BEGIN IMMEDIATE`.** judgment:
  rejected in ADR 0083 and unchanged here — a rule that fires everywhere carries
  no information about where the hazard is.
- **Do nothing; rely on review.** verified: four call sites were converted one at
  a time as separate flakes surfaced (ADR 0083), and #546 is the fifth. judgment:
  review has had five chances at this and the defect class is still live.
- **A runtime `debug_assert!` in a transaction wrapper that tracks statement
  order.** judgment: it only reaches paths a test exercises, and it moves a
  static property into the hot path; the boundary check costs nothing at runtime
  and covers every call site whether tested or not.
