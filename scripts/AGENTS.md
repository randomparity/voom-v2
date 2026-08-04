# Guard-script-specific agent guidance

This file supplements the repository-root `AGENTS.md` for repository guard scripts.

## Structural enforcement

- A zero-tolerance Rust boundary guard must analyze syntax structurally. Whole-text regexes
  are insufficient for paths, generics, nested import trees, aliases, comments, raw
  identifiers, lexical shadowing, and macro token trees.
- Every reported bypass becomes a self-test fixture before the implementation changes. Cover
  direct paths, aliases and re-exports, nested modules, wildcards and `self`, generic tuple
  types, raw identifiers, arbitrary macros, and paths synthesized from macro metavariables.
- Pair forbidden fixtures with compiling safe controls for strings, comments, type-only uses,
  and shadowing. A guard that closes bypasses by rejecting unrelated valid Rust is not done
  unless that conservative policy is deliberate and documented.
- Test fixtures must compile when the claim depends on valid Rust syntax. Assert both isolated
  diagnostics and aggregate counts so a new detector cannot hide a missed case behind duplicate
  findings.
- Keep guard output deterministic and hook-safe: use repository-relative paths, avoid
  cross-file or cross-scope alias leakage, and run the self-test plus the production-tree guard
  through their `just`/`prek` entry points after every change.
