use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "voom", version, about = "VOOM control plane CLI", long_about = None)]
pub struct Cli {
    /// Override the database URL (default: XDG data dir).
    #[arg(long, env = "VOOM_DATABASE_URL", global = true)]
    pub database_url: Option<String>,

    /// Log level (error|warn|info|debug|trace).
    #[arg(long, default_value = "info", global = true, env = "VOOM_LOG_LEVEL")]
    pub log_level: String,

    /// Log format on stderr (text|json). Defaults to json so logs and command
    /// output are both machine-parseable.
    #[arg(
        long,
        value_enum,
        default_value_t = LogFormatArg::Json,
        global = true,
        env = "VOOM_LOG_FORMAT"
    )]
    pub log_format: LogFormatArg,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print build version, semver, git SHA, and dirty flag.
    Version,
    /// Report database health without applying migrations.
    Health,
    /// Apply pending migrations idempotently.
    Init,
    /// Generate or inspect execution plans.
    #[command(subcommand)]
    Plan(PlanCommand),
    /// Generate, apply, or execute compliance reports.
    #[command(subcommand)]
    Compliance(ComplianceCommand),
    /// Manage policy documents and input sets.
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Register and manage execution nodes.
    #[command(subcommand)]
    Node(NodeCommand),
    /// Manage durable video encode profiles.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Manage durable quality scoring profiles.
    #[command(subcommand)]
    ScoringProfile(ScoringProfileCommand),
    /// Register and inspect workers.
    #[command(subcommand)]
    Worker(WorkerCommand),
    /// Inspect scheduler state.
    #[command(subcommand)]
    Scheduler(SchedulerCommand),
    /// Inspect the append-only durable event journal.
    #[command(subcommand)]
    Event(EventCommand),
    /// Inspect durable jobs (operator-initiated units of work).
    #[command(subcommand)]
    Job(JobCommand),
    /// Inspect durable tickets (scheduled units of execution).
    #[command(subcommand)]
    Ticket(TicketCommand),
    /// Stage, verify, commit, or inspect artifacts.
    #[command(subcommand)]
    Artifact(ArtifactCommand),
    /// Scan an explicit path (`--path`) or a configured library root (`--root`).
    Scan {
        #[arg(long, required_unless_present = "root", conflicts_with = "root")]
        path: Option<PathBuf>,
        /// Scan the enabled library root with this id. A disabled root or
        /// library is refused (`BLOCKED`), not scanned.
        #[arg(long, conflicts_with = "path")]
        root: Option<u64>,
    },
    /// List and inspect asset bundles and their members.
    #[command(subcommand)]
    Bundle(BundleCommand),
    /// List and inspect durable backup records.
    #[command(subcommand)]
    Backup(BackupCommand),
    /// Manage libraries and their roots (durable scan configuration).
    #[command(subcommand)]
    Library(LibraryCommand),
    /// Manage durable scheduling policy records.
    #[command(subcommand)]
    SchedulingPolicy(SchedulingPolicyCommand),
    /// Manage durable safety policy records.
    #[command(subcommand)]
    SafetyPolicy(SafetyPolicyCommand),
    /// List, inspect, and transition durable issues.
    #[command(subcommand)]
    Issue(IssueCommand),
    /// Acquire, release, force-release, and list manual use-lease locks.
    #[command(subcommand)]
    Lease(LeaseCommand),
}

#[derive(Subcommand, Debug, Clone)]
pub enum IssueCommand {
    /// List issues, filtered and keyset-paginated by ascending id.
    List {
        #[arg(long)]
        status: Option<IssueStatusArg>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        priority: Option<IssuePriorityArg>,
        #[arg(long)]
        severity: Option<IssueSeverityArg>,
        /// Return only issues with id greater than this cursor (exclusive).
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one issue by id.
    Show {
        #[arg(long)]
        issue_id: u64,
    },
    /// Override an issue's priority (stamps `priority_source = user`).
    Update {
        #[arg(long)]
        issue_id: u64,
        #[arg(long)]
        priority: IssuePriorityArg,
        #[arg(long)]
        priority_reason: Option<String>,
    },
    /// Resolve an issue.
    Resolve {
        #[arg(long)]
        issue_id: u64,
    },
    /// Suppress an issue for a number of days from now.
    Suppress {
        #[arg(long)]
        issue_id: u64,
        /// Days from now; capped at 100 years so the horizon cannot overflow
        /// `OffsetDateTime` (which would panic rather than emit an envelope).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=36_500))]
        days: u32,
    },
    /// Accept an issue (acknowledge without acting).
    Accept {
        #[arg(long)]
        issue_id: u64,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum LeaseCommand {
    /// Acquire a blocking manual lock on a scope. A live manual lock fails any
    /// commit whose affected scope it overlaps, until it is released.
    Acquire {
        #[arg(long)]
        scope_type: LeaseScopeTypeArg,
        #[arg(long)]
        scope_id: u64,
        /// Identifies who holds the lock (operator name, ticket ref, ...).
        #[arg(long)]
        issuer_ref: String,
    },
    /// Release a manual lock you hold.
    Release {
        #[arg(long)]
        lease_id: u64,
    },
    /// Force-release a manual lock held by someone else. Records an audited
    /// override naming the actor and reason.
    ForceRelease {
        #[arg(long)]
        lease_id: u64,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
    /// List live manual locks with their age (for forgotten-hold spotting).
    List,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum IssueStatusArg {
    Open,
    Planned,
    Resolved,
    Suppressed,
    Accepted,
}

impl IssueStatusArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::issues::IssueStatus {
        use voom_store::repo::issues::IssueStatus;
        match self {
            Self::Open => IssueStatus::Open,
            Self::Planned => IssueStatus::Planned,
            Self::Resolved => IssueStatus::Resolved,
            Self::Suppressed => IssueStatus::Suppressed,
            Self::Accepted => IssueStatus::Accepted,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum IssuePriorityArg {
    Urgent,
    High,
    Normal,
    Low,
    Someday,
}

impl IssuePriorityArg {
    #[must_use]
    pub const fn to_core(self) -> voom_core::IssuePriority {
        use voom_core::IssuePriority;
        match self {
            Self::Urgent => IssuePriority::Urgent,
            Self::High => IssuePriority::High,
            Self::Normal => IssuePriority::Normal,
            Self::Low => IssuePriority::Low,
            Self::Someday => IssuePriority::Someday,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum IssueSeverityArg {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl IssueSeverityArg {
    #[must_use]
    pub const fn to_core(self) -> voom_core::IssueSeverity {
        use voom_core::IssueSeverity;
        match self {
            Self::Critical => IssueSeverity::Critical,
            Self::High => IssueSeverity::High,
            Self::Medium => IssueSeverity::Medium,
            Self::Low => IssueSeverity::Low,
            Self::Info => IssueSeverity::Info,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum LeaseScopeTypeArg {
    Asset,
    Bundle,
    Version,
    Location,
}

impl LeaseScopeTypeArg {
    /// Pair the scope-type discriminant with a raw id into a `LeaseScope`.
    #[must_use]
    pub fn to_scope(self, id: u64) -> voom_store::repo::LeaseScope {
        use voom_core::{BundleId, FileAssetId, FileLocationId, FileVersionId};
        use voom_store::repo::LeaseScope;
        match self {
            Self::Asset => LeaseScope::Asset(FileAssetId(id)),
            Self::Bundle => LeaseScope::Bundle(BundleId(id)),
            Self::Version => LeaseScope::Version(FileVersionId(id)),
            Self::Location => LeaseScope::Location(FileLocationId(id)),
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum SchedulingPolicyCommand {
    /// Create a scheduling policy.
    Create {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        display_name: String,
        #[arg(long)]
        priority: SchedulePriorityArg,
        /// Optional copy window as `HH:MM-HH:MM` (24-hour).
        #[arg(long)]
        copy_window: Option<String>,
        #[arg(long)]
        large_jobs_night_only: bool,
        #[arg(long)]
        pause_on_degraded_node: bool,
    },
    /// List scheduling policies.
    List,
    /// Show one scheduling policy by slug.
    Show {
        #[arg(long)]
        slug: String,
    },
    /// Replace every mutable field of an existing scheduling policy.
    Update {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        display_name: String,
        #[arg(long)]
        priority: SchedulePriorityArg,
        #[arg(long)]
        copy_window: Option<String>,
        #[arg(long)]
        large_jobs_night_only: bool,
        #[arg(long)]
        pause_on_degraded_node: bool,
    },
    /// Delete a scheduling policy by slug.
    Delete {
        #[arg(long)]
        slug: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum SafetyPolicyCommand {
    /// Create a safety policy.
    Create(SafetyPolicyFields),
    /// List safety policies.
    List,
    /// Show one safety policy by slug.
    Show {
        #[arg(long)]
        slug: String,
    },
    /// Replace every mutable field of an existing safety policy.
    Update(SafetyPolicyFields),
    /// Delete a safety policy by slug.
    Delete {
        #[arg(long)]
        slug: String,
    },
}

/// The full mutable field set of a safety policy, shared by `create` and the
/// full-replace `update`. Booleans default to false (absent flag) and the two
/// list-valued flags are repeatable.
#[expect(
    clippy::struct_excessive_bools,
    reason = "CLI mirror of the safety policy's four independent spec-mandated toggles"
)]
#[derive(clap::Args, Debug, Clone)]
pub struct SafetyPolicyFields {
    #[arg(long)]
    pub slug: String,
    #[arg(long)]
    pub display_name: String,
    /// Operation kinds the daemon may auto-execute (repeatable). Wire tokens,
    /// e.g. `remux`, `transcode_video`.
    #[arg(long = "auto-execute-operation")]
    pub auto_execute_operations: Vec<String>,
    #[arg(long)]
    pub backup_required: bool,
    #[arg(long)]
    pub approval_required: bool,
    /// Allowed commit modes (repeatable).
    #[arg(long = "allowed-commit-mode")]
    pub allowed_commit_modes: Vec<CommitModeArg>,
    #[arg(long, default_value_t = VerificationLevelArg::None)]
    pub verification_level: VerificationLevelArg,
    #[arg(long)]
    pub block_on_failed_records: bool,
    #[arg(long)]
    pub block_on_recovery_required_records: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum SchedulePriorityArg {
    NewestFirst,
    OldestFirst,
    SmallestFirst,
    LargestFirst,
}

impl SchedulePriorityArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::scheduling_policies::SchedulePriority {
        use voom_store::repo::scheduling_policies::SchedulePriority;
        match self {
            Self::NewestFirst => SchedulePriority::NewestFirst,
            Self::OldestFirst => SchedulePriority::OldestFirst,
            Self::SmallestFirst => SchedulePriority::SmallestFirst,
            Self::LargestFirst => SchedulePriority::LargestFirst,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum CommitModeArg {
    AddOnly,
    Replace,
    Delete,
    Archive,
}

impl CommitModeArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::safety_policies::CommitMode {
        use voom_store::repo::safety_policies::CommitMode;
        match self {
            Self::AddOnly => CommitMode::AddOnly,
            Self::Replace => CommitMode::Replace,
            Self::Delete => CommitMode::Delete,
            Self::Archive => CommitMode::Archive,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum VerificationLevelArg {
    None,
    QuickDecode,
    Full,
}

impl VerificationLevelArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::safety_policies::VerificationLevel {
        use voom_store::repo::safety_policies::VerificationLevel;
        match self {
            Self::None => VerificationLevel::None,
            Self::QuickDecode => VerificationLevel::QuickDecode,
            Self::Full => VerificationLevel::Full,
        }
    }
}

impl std::fmt::Display for VerificationLevelArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_store().as_str())
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum LibraryCommand {
    /// Create a library.
    Add {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        display_name: String,
        #[arg(long, default_value_t = LibraryMediaKindArg::Unknown)]
        media_kind: LibraryMediaKindArg,
        #[arg(long)]
        description: Option<String>,
        /// Create the library disabled.
        #[arg(long)]
        disabled: bool,
    },
    /// List libraries.
    List,
    /// Show one library.
    Show {
        #[arg(long)]
        library_id: u64,
    },
    /// Update a library's mutable attributes.
    Update {
        #[arg(long)]
        library_id: u64,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        media_kind: Option<LibraryMediaKindArg>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Enable a library.
    Enable {
        #[arg(long)]
        library_id: u64,
    },
    /// Disable a library (its roots refuse scans until re-enabled).
    Disable {
        #[arg(long)]
        library_id: u64,
    },
    /// Delete a library and cascade its roots.
    Remove {
        #[arg(long)]
        library_id: u64,
    },
    /// Set or clear a library's default quality scoring profile.
    SetDefaultScoringProfile {
        #[arg(long)]
        library_id: u64,
        /// Scoring profile name to set as the default. Omit with `--clear` to
        /// remove the default.
        #[arg(long, required_unless_present = "clear", conflicts_with = "clear")]
        scoring_profile: Option<String>,
        /// Clear the library's default scoring profile.
        #[arg(long)]
        clear: bool,
    },
    /// Manage library roots.
    #[command(subcommand)]
    Root(LibraryRootCommand),
}

#[derive(Subcommand, Debug, Clone)]
pub enum LibraryRootCommand {
    /// Add a root to a library. `--path` is canonicalized and stored; a
    /// symlinked path is rejected.
    Add(LibraryRootAddArgs),
    /// List roots, optionally filtered to one library.
    List {
        #[arg(long)]
        library_id: Option<u64>,
    },
    /// Show one root.
    Show {
        #[arg(long)]
        root_id: u64,
    },
    /// Update a root's mutable discovery settings.
    Update(LibraryRootUpdateArgs),
    /// Enable a root.
    Enable {
        #[arg(long)]
        root_id: u64,
    },
    /// Disable a root (refuses scans until re-enabled).
    Disable {
        #[arg(long)]
        root_id: u64,
    },
    /// Delete a root.
    Remove {
        #[arg(long)]
        root_id: u64,
    },
}

#[derive(Args, Debug, Clone)]
pub struct LibraryRootAddArgs {
    #[arg(long)]
    pub library_id: u64,
    #[arg(long)]
    pub path: PathBuf,
    #[arg(long, default_value_t = LibraryRootKindArg::LocalPath)]
    pub root_kind: LibraryRootKindArg,
    #[arg(long, default_value_t = LibraryScanModeArg::ManualRecursive)]
    pub scan_mode: LibraryScanModeArg,
    #[arg(long = "include-glob")]
    pub include_glob: Vec<String>,
    #[arg(long = "exclude-glob")]
    pub exclude_glob: Vec<String>,
    /// Primary-media extension allowlist. Empty = the built-in default set.
    #[arg(long = "extension")]
    pub extension: Vec<String>,
    #[arg(long, default_value_t = SymlinkPolicyArg::Reject)]
    pub symlink_policy: SymlinkPolicyArg,
    #[arg(long, default_value_t = HiddenFilePolicyArg::Ignore)]
    pub hidden_file_policy: HiddenFilePolicyArg,
    #[arg(long)]
    pub max_depth: Option<u32>,
    #[arg(long, default_value_t = 0)]
    pub stability_seconds: u32,
    #[arg(long, default_value_t = 0)]
    pub debounce_seconds: u32,
    #[arg(long)]
    pub output_root: Option<String>,
    #[arg(long)]
    pub staging_root: Option<String>,
    #[arg(long)]
    pub backup_root: Option<String>,
    /// Create the root disabled.
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Args, Debug, Clone)]
pub struct LibraryRootUpdateArgs {
    #[arg(long)]
    pub root_id: u64,
    #[arg(long = "include-glob")]
    pub include_glob: Option<Vec<String>>,
    #[arg(long = "exclude-glob")]
    pub exclude_glob: Option<Vec<String>>,
    #[arg(long = "extension")]
    pub extension: Option<Vec<String>>,
    #[arg(long)]
    pub scan_mode: Option<LibraryScanModeArg>,
    #[arg(long)]
    pub symlink_policy: Option<SymlinkPolicyArg>,
    #[arg(long)]
    pub hidden_file_policy: Option<HiddenFilePolicyArg>,
    #[arg(long)]
    pub max_depth: Option<u32>,
    #[arg(long)]
    pub stability_seconds: Option<u32>,
    #[arg(long)]
    pub debounce_seconds: Option<u32>,
    #[arg(long)]
    pub output_root: Option<String>,
    #[arg(long)]
    pub staging_root: Option<String>,
    #[arg(long)]
    pub backup_root: Option<String>,
}

macro_rules! value_enum_to_store {
    ($arg:ident => $store:path { $($variant:ident),+ $(,)? }) => {
        #[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
        #[value(rename_all = "snake_case")]
        pub enum $arg {
            $($variant),+
        }

        impl $arg {
            #[must_use]
            pub const fn to_store(self) -> $store {
                match self {
                    $(Self::$variant => <$store>::$variant),+
                }
            }
        }

        // Display mirrors the store vocabulary exactly by delegating to the
        // store enum's `as_str()`, so the wire strings live in one place.
        impl std::fmt::Display for $arg {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.to_store().as_str())
            }
        }
    };
}

value_enum_to_store!(LibraryMediaKindArg => voom_store::repo::library::libraries::LibraryMediaKind {
    Movie,
    Episode,
    Personal,
    Unknown,
});
value_enum_to_store!(LibraryRootKindArg => voom_store::repo::library::library_roots::LibraryRootKind {
    LocalPath,
    SharedMount,
});
value_enum_to_store!(LibraryScanModeArg => voom_store::repo::library::library_roots::LibraryScanMode {
    ExplicitOnly,
    ManualRecursive,
    WatchEnabled,
});
value_enum_to_store!(SymlinkPolicyArg => voom_store::repo::library::library_roots::SymlinkPolicy {
    Reject,
    Follow,
});
value_enum_to_store!(HiddenFilePolicyArg => voom_store::repo::library::library_roots::HiddenFilePolicy {
    Ignore,
    Include,
});

#[derive(Subcommand, Debug, Clone)]
pub enum BackupCommand {
    /// List backup records, optionally filtered by status.
    List {
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long)]
        status: Option<BackupStatusArg>,
        /// Keyset cursor: return backups after this id (`next_cursor` from a
        /// prior page). See ADR 0031.
        #[arg(long)]
        after_id: Option<u64>,
    },
    /// Show one backup record.
    Show {
        #[arg(long)]
        backup_id: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum BackupStatusArg {
    Pending,
    Verified,
    Failed,
}

impl BackupStatusArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::backups::BackupStatus {
        match self {
            Self::Pending => voom_store::repo::backups::BackupStatus::Pending,
            Self::Verified => voom_store::repo::backups::BackupStatus::Verified,
            Self::Failed => voom_store::repo::backups::BackupStatus::Failed,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum BundleCommand {
    /// List asset bundles with their member counts.
    List {
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Keyset cursor: return bundles after this id (`next_cursor` from a
        /// prior page). See ADR 0031.
        #[arg(long)]
        after_id: Option<u64>,
    },
    /// Show one bundle: members, roles, and media work/variant lineage.
    Show {
        #[arg(long)]
        bundle_id: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum PlanCommand {
    /// Generate a plan from a policy file and built-in input fixture.
    DryRun {
        #[arg(long)]
        policy_file: PathBuf,
        #[arg(long)]
        input_fixture: String,
    },
    /// Generate a plan from durable accepted policy and input rows.
    Show {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum PolicyCommand {
    /// Manage policy input sets.
    #[command(subcommand)]
    Input(PolicyInputCommand),
    /// Create a policy document (with its initial accepted version) from a .voom file.
    Create {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Manage versions of an existing policy document.
    #[command(subcommand)]
    Version(PolicyVersionCommand),
    /// List policy documents.
    List,
    /// Show one policy document and its versions.
    Show {
        #[arg(long)]
        document_id: u64,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum PolicyVersionCommand {
    /// Add a new accepted version to an existing document from a .voom file.
    Add {
        #[arg(long)]
        document_id: u64,
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum PolicyInputCommand {
    /// Create a policy input set from scan-created rows. Pass `--all` to cover
    /// every live video file, or the full single-file arg set for one file.
    CreateFromScan {
        #[arg(long)]
        slug: String,
        /// Whole-library mode: build from all live video file-versions.
        #[arg(long, conflicts_with_all = ["root", "file_version_id", "media_snapshot_id", "container", "video_codec"])]
        all: bool,
        /// Root-scoped mode: build from live video file-versions under this
        /// library root's canonical path.
        #[arg(long, conflicts_with_all = ["file_version_id", "media_snapshot_id", "container", "video_codec"])]
        root: Option<u64>,
        #[arg(long, requires_all = ["media_snapshot_id", "container", "video_codec"])]
        file_version_id: Option<u64>,
        #[arg(long)]
        media_snapshot_id: Option<u64>,
        #[arg(long)]
        container: Option<String>,
        #[arg(long)]
        video_codec: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ComplianceCommand {
    /// Generate a compliance report from durable policy and input rows
    /// (preview: `--policy-version-id` + `--input-set-id`), or read a completed
    /// run's durable per-phase chain (`--job-id`). Exactly one mode.
    Report {
        #[arg(long, requires = "input_set_id", conflicts_with = "job_id")]
        policy_version_id: Option<u64>,
        #[arg(long, requires = "policy_version_id", conflicts_with = "job_id")]
        input_set_id: Option<u64>,
        #[arg(long, conflicts_with_all = ["policy_version_id", "input_set_id"])]
        job_id: Option<u64>,
    },
    /// Apply compliance report findings to durable issues.
    Apply {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
    },
    /// Apply issues, then execute supported compliance work.
    Execute {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
        #[arg(long)]
        staging_root: Option<std::path::PathBuf>,
        #[arg(long)]
        output_dir: Option<std::path::PathBuf>,
        /// Slug of a safety policy to enforce fail-closed before dispatch. When
        /// omitted the manual execute path is unchanged.
        #[arg(long)]
        safety_policy: Option<String>,
        /// Backup-before-mutation destination root. Required when the safety
        /// policy sets `backup_required`.
        #[arg(long)]
        backup_root: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum NodeCommand {
    /// Register a node and print its one-time bearer token.
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        kind: NodeKindArg,
        #[arg(long)]
        heartbeat_ttl_seconds: Option<u32>,
    },
    /// Record a node heartbeat using exactly one token source.
    Heartbeat {
        #[arg(long)]
        node_id: u64,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        token_env: Option<String>,
        #[arg(long)]
        token_stdin: bool,
    },
    /// List nodes, optionally filtered by status.
    List {
        #[arg(long)]
        status: Option<NodeStatusArg>,
    },
    /// Show one node.
    Show {
        #[arg(long)]
        node_id: u64,
    },
    /// Retire a node using an expected epoch.
    Retire {
        #[arg(long)]
        node_id: u64,
        #[arg(long)]
        expected_epoch: u64,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProfileCommand {
    /// List the active video encode profiles.
    List,
    /// Show one video encode profile by name.
    Show {
        #[arg(long)]
        name: String,
    },
    /// Create a durable video encode profile. The target codec is derived from
    /// the encoder; every field is validated against the encoder's capabilities.
    Create(VideoProfileFields),
    /// Replace every mutable field of an existing video encode profile.
    Update(VideoProfileFields),
    /// Soft-retire a video encode profile (hidden from `list`, still resolvable).
    Retire {
        #[arg(long)]
        name: String,
    },
}

#[derive(clap::Args, Debug, Clone)]
pub struct VideoProfileFields {
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub encoder: String,
    #[arg(long)]
    pub crf: u8,
    #[arg(long)]
    pub preset: String,
    #[arg(long)]
    pub tune: Option<String>,
    #[arg(long)]
    pub codec_profile: Option<String>,
    #[arg(long)]
    pub codec_level: Option<String>,
    #[arg(long)]
    pub pixel_format: Option<String>,
    #[arg(long)]
    pub max_width: Option<u32>,
    #[arg(long)]
    pub max_height: Option<u32>,
    #[arg(long, default_value = "mkv")]
    pub output_container: String,
    #[arg(long)]
    pub copy_compatible: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ScoringProfileCommand {
    /// Create a quality scoring profile. `--definition` is a JSON object.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 1)]
        version: u32,
        #[arg(long, default_value = "{}")]
        definition: String,
    },
    /// List active quality scoring profiles.
    List,
    /// Show one quality scoring profile by name.
    Show {
        #[arg(long)]
        name: String,
    },
    /// Replace an existing quality scoring profile's version and definition.
    Update {
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 1)]
        version: u32,
        #[arg(long, default_value = "{}")]
        definition: String,
    },
    /// Soft-retire a quality scoring profile (hidden from `list`, still resolvable).
    Retire {
        #[arg(long)]
        name: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum WorkerCommand {
    /// Register a worker for a node using exactly one node token source.
    Register {
        #[arg(long)]
        node_id: u64,
        #[arg(long)]
        name: String,
        #[arg(long)]
        kind: WorkerKindArg,
        #[arg(long)]
        capability: Vec<String>,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        token_env: Option<String>,
        #[arg(long)]
        token_stdin: bool,
    },
    /// List workers, optionally filtered by status.
    List {
        #[arg(long)]
        status: Option<WorkerStatusArg>,
    },
    /// Show one worker.
    Show {
        #[arg(long)]
        worker_id: u64,
    },
    /// Launch a bundled mutation worker locally, register it, and supervise it (foreground).
    RunLocal {
        #[arg(long)]
        kind: LocalWorkerKindArg,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum LocalWorkerKindArg {
    Ffmpeg,
    Mkvtoolnix,
}

impl LocalWorkerKindArg {
    #[must_use]
    pub const fn to_control_plane(self) -> voom_control_plane::LocalWorkerKind {
        match self {
            Self::Ffmpeg => voom_control_plane::LocalWorkerKind::Ffmpeg,
            Self::Mkvtoolnix => voom_control_plane::LocalWorkerKind::Mkvtoolnix,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum SchedulerCommand {
    /// Inspect scheduler decisions.
    #[command(subcommand)]
    Decisions(SchedulerDecisionCommand),
    /// Inspect scheduler execution leases (the `leases` table). Distinct from
    /// the operator use-lease `voom lease` command.
    #[command(subcommand)]
    Leases(SchedulerLeaseCommand),
}

#[derive(Subcommand, Debug, Clone)]
pub enum SchedulerLeaseCommand {
    /// List scheduler leases, optionally filtered by state.
    List {
        #[arg(long)]
        state: Option<LeaseStateArg>,
        /// Keyset cursor: return leases after this id (`next_cursor` from a
        /// prior page). See ADR 0031.
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one scheduler lease.
    Show {
        #[arg(long)]
        lease_id: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum LeaseStateArg {
    Held,
    Released,
    Expired,
    ForceReleased,
}

impl LeaseStateArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::leases::LeaseState {
        use voom_store::repo::leases::LeaseState;
        match self {
            Self::Held => LeaseState::Held,
            Self::Released => LeaseState::Released,
            Self::Expired => LeaseState::Expired,
            Self::ForceReleased => LeaseState::ForceReleased,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum EventCommand {
    /// List durable events, newest first. Filter by kind, subject, and an
    /// occurred-at window; page with `--after-id` (ADR 0031).
    List {
        /// Event kind wire token, e.g. `ticket.leased` (see the event taxonomy).
        #[arg(long)]
        kind: Option<String>,
        /// Subject-type wire token, e.g. `ticket`, `lease`, `node`.
        #[arg(long)]
        subject_type: Option<String>,
        /// Subject id (requires the matching `--subject-type` to be meaningful).
        #[arg(long)]
        subject_id: Option<u64>,
        /// Inclusive lower bound on occurred-at (RFC 3339, e.g.
        /// `2026-07-02T00:00:00Z`).
        #[arg(long)]
        since: Option<String>,
        /// Inclusive upper bound on occurred-at (RFC 3339).
        #[arg(long)]
        until: Option<String>,
        /// Keyset cursor: return events after this id (`next_cursor` from a
        /// prior page).
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one event by id.
    Show {
        #[arg(long)]
        event_id: u64,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum JobCommand {
    /// List durable jobs, newest first, optionally filtered by state.
    List {
        #[arg(long)]
        state: Option<JobStateArg>,
        /// Keyset cursor: return jobs after this id (`next_cursor` from a prior
        /// page). See ADR 0031.
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one job by id.
    Show {
        #[arg(long)]
        job_id: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum JobStateArg {
    Open,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStateArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::jobs::JobState {
        use voom_store::repo::jobs::JobState;
        match self {
            Self::Open => JobState::Open,
            Self::Succeeded => JobState::Succeeded,
            Self::Failed => JobState::Failed,
            Self::Cancelled => JobState::Cancelled,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum TicketCommand {
    /// List durable tickets, newest first, optionally filtered by state.
    List {
        #[arg(long)]
        state: Option<TicketStateArg>,
        /// Keyset cursor: return tickets after this id (`next_cursor` from a
        /// prior page). See ADR 0031.
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one ticket by id.
    Show {
        #[arg(long)]
        ticket_id: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum TicketStateArg {
    Pending,
    Ready,
    Leased,
    Succeeded,
    Failed,
}

impl TicketStateArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::tickets::TicketState {
        use voom_store::repo::tickets::TicketState;
        match self {
            Self::Pending => TicketState::Pending,
            Self::Ready => TicketState::Ready,
            Self::Leased => TicketState::Leased,
            Self::Succeeded => TicketState::Succeeded,
            Self::Failed => TicketState::Failed,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum ArtifactCommand {
    /// Copy a scanned file version into a staging path.
    StageCopy {
        #[arg(long)]
        file_version_id: u64,
        #[arg(long)]
        source_location_id: Option<u64>,
        #[arg(long)]
        staging_path: PathBuf,
    },
    /// Verify the live staging bytes for an artifact handle.
    Verify {
        #[arg(long)]
        artifact_handle_id: u64,
        /// Staging directory the artifact must reside within. The worker
        /// rejects any artifact path not contained by this root.
        #[arg(long)]
        staging_root: PathBuf,
    },
    /// Promote a verified staged artifact to an add-only target path.
    Commit {
        #[arg(long)]
        artifact_handle_id: u64,
        #[arg(long)]
        target_path: PathBuf,
    },
    /// Re-drive a commit left in `recovery_required` back to completion.
    RecoverCommit {
        #[arg(long)]
        artifact_handle_id: u64,
    },
    /// List artifact handles, optionally filtered by inspection state.
    List {
        #[arg(long)]
        state: Option<ArtifactStateArg>,
        /// Keyset cursor: return handles after this id (`next_cursor` from a
        /// prior page). See ADR 0031.
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one artifact handle.
    Show {
        #[arg(long)]
        artifact_handle_id: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum ArtifactStateArg {
    Staged,
    Verified,
    Committed,
    Failed,
    RecoveryRequired,
}

#[derive(Subcommand, Debug, Clone)]
pub enum SchedulerDecisionCommand {
    /// List scheduler decisions.
    List {
        #[arg(long)]
        ticket_id: Option<u64>,
        #[arg(long)]
        worker_id: Option<u64>,
        #[arg(long)]
        node_id: Option<u64>,
        #[arg(long)]
        outcome: Option<SchedulerDecisionOutcomeArg>,
        /// Keyset cursor: return decisions after this id (`next_cursor` from a
        /// prior page). See ADR 0031.
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one scheduler decision.
    Show {
        #[arg(long)]
        decision_id: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum SchedulerDecisionOutcomeArg {
    Selected,
    Idle,
    NoEligibleCandidate,
    Rejected,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum NodeKindArg {
    Local,
    Remote,
    Synthetic,
}

impl NodeKindArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::nodes::NodeKind {
        match self {
            Self::Local => voom_store::repo::nodes::NodeKind::Local,
            Self::Remote => voom_store::repo::nodes::NodeKind::Remote,
            Self::Synthetic => voom_store::repo::nodes::NodeKind::Synthetic,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum NodeStatusArg {
    Registered,
    Active,
    Stale,
    Retired,
}

impl NodeStatusArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::nodes::NodeStatus {
        match self {
            Self::Registered => voom_store::repo::nodes::NodeStatus::Registered,
            Self::Active => voom_store::repo::nodes::NodeStatus::Active,
            Self::Stale => voom_store::repo::nodes::NodeStatus::Stale,
            Self::Retired => voom_store::repo::nodes::NodeStatus::Retired,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum WorkerKindArg {
    Local,
    Remote,
    Synthetic,
}

impl WorkerKindArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::workers::WorkerKind {
        match self {
            Self::Local => voom_store::repo::workers::WorkerKind::Local,
            Self::Remote => voom_store::repo::workers::WorkerKind::Remote,
            Self::Synthetic => voom_store::repo::workers::WorkerKind::Synthetic,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum WorkerStatusArg {
    Registered,
    Active,
    Stale,
    Retired,
}

impl WorkerStatusArg {
    #[must_use]
    pub const fn to_store(self) -> voom_store::repo::workers::WorkerStatus {
        match self {
            Self::Registered => voom_store::repo::workers::WorkerStatus::Registered,
            Self::Active => voom_store::repo::workers::WorkerStatus::Active,
            Self::Stale => voom_store::repo::workers::WorkerStatus::Stale,
            Self::Retired => voom_store::repo::workers::WorkerStatus::Retired,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum LogFormatArg {
    Text,
    Json,
}

impl LogFormatArg {
    #[must_use]
    pub fn to_core(self) -> voom_core::LogFormat {
        match self {
            Self::Text => voom_core::LogFormat::Text,
            Self::Json => voom_core::LogFormat::Json,
        }
    }

    /// Canonical lowercase name accepted by `voom_core::LogFormat::parse`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}
