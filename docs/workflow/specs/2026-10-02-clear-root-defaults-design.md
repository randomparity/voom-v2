# Clear library-root defaults (#662)

## Problem and scope

Operators can set output, staging, and backup root defaults but cannot clear them.
The retirement guard refuses a root still named by another root, so this prevents
retirement without a replacement. Issue #662 and the approved campaign scope own this change.

## Design and success

Extend `LibraryRootUpdateArgs` with `--clear-output-root`, `--clear-staging-root`,
and `--clear-backup-root`; each conflicts with its corresponding set flag through clap.
Extend the existing CLI adapter: absent set/clear maps to `None`, set maps to
`Some(Some(StorageRootId(id)))`, and clear maps to `Some(None)`. Different defaults
remain independent. Preserve the control-plane/store ownership and error propagation.
The existing `Option<Option<StorageRootId>>` update contract needs no new capability.

A clear returns the existing JSON envelope with that default null. Omitted defaults
retain their values. Once the three references to a target are cleared, retirement
succeeds through the existing guard. A corresponding set/clear pair fails with exit 1
and a single `BAD_ARGS` envelope before database access. Help describes each clear flag.

No ownership move, schema migration, new dependency, or ADR is warranted: this exposes
an existing store operation using the issue's proposed CLI shape. A sentinel root ID
would overload valid numeric input; a new subcommand would duplicate the update adapter.

## Global Constraints

Use existing Rust/clap dependencies and repository toolchain; change no version floors.
Preserve domain newtypes and the CLI single-envelope contract. Use sibling-file unit tests
or existing integration suites. Never pair paused tokio time with a real SQLite pool.

## Failure model

- Actors and deployments: local CLI operators and automation using an existing database.
- Invariants and assets: output/staging/backup defaults change only when requested;
  conflicting intent is rejected before mutation; retirement continues through its guard.
- Accepted failure classes: missing/invalid roots and inaccessible/corrupt databases fail
  through existing control-plane/store errors; this change does not promise their repair.
- Covered elsewhere: relationship/owner validation (#616); pending-artifact retirement
  (#663); unrelated setting-clear controls (outside this issue).

## Threat model

- Boundary inventory: no new boundary; existing command-line input accepts three booleans.
- Actor model: local callers select arguments and database under their existing authority.
- Control per boundary: clap rejects corresponding set/clear conflicts; root IDs keep
  existing numeric parsing and repository validation; errors use the existing envelope.
- Out of scope: new authorization and storage-validation policies; existing owners retain them.

## Validation

CLI integration tests exercise setting three defaults, an omitted-default update,
sequential independent clears, persisted readback, retirement refusal before clearing
and success afterward. A parser test covers three conflicting pairs in both orders,
using an inaccessible database URL to ensure BAD_ARGS wins before database access.
Run focused red/green proofs, source guards, formatting, clippy, commit hooks, and final
`just ci`. Test fixtures retain and clean temporary directories. No timing/resource
sensitivity is introduced; existing suite owns broader resource constraints.
