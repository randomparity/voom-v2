# Workflow static checks

Run `just setup-workflow-tools` in a fresh worktree, then `just check-workflows`.
`just setup` includes provisioning. CI source jobs and the workflow prek hook use
the same recipes. The guard also runs through `just source-checks` and `just ci`.

Provisioning downloads official actionlint 1.7.12, ShellCheck 0.11.0, and zizmor
1.30.1 release archives, verifies pinned SHA256 digests, and installs only their
executables under ignored `target/workflow-tools/`. macOS and Linux arm64/x86_64
are supported; curl, tar, and sha256sum or shasum are required. No compiler is
installed. Running setup again replaces the repository-local tools. After
`cargo clean`, provision again. Update version URLs and platform digests together.

Actionlint checks workflow structure, expressions, and shell bodies with mandatory
ShellCheck. Zizmor runs its regular offline audits with normal severity/exit
behavior and strict collection. Neither service credentials nor a GitHub token
are required. This does not prove runtime service permissions or execute shell
bodies; network-dependent audits are omitted. The self-test uses safe and broken
workflow controls and rejects missing tools and a corrupt download.

The first stable-tool run found five floating just versions, release tag
interpolation, release cache poisoning, and one informational release-action
suggestion. The fixes pin just 1.58.0, read the tag through a quoted environment
variable, and remove the release cache. The existing draft-release action is
retained with one narrow suppression to preserve multi-target draft updates.
The remaining 21 persona-suppressed observations are the tool's regular defaults,
not repository-wide ignores. See the #621 design for the failure model.
