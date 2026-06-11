use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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
    /// Inspect seeded video encode profiles.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Register and inspect workers.
    #[command(subcommand)]
    Worker(WorkerCommand),
    /// Inspect scheduler state.
    #[command(subcommand)]
    Scheduler(SchedulerCommand),
    /// Stage, verify, commit, or inspect artifacts.
    #[command(subcommand)]
    Artifact(ArtifactCommand),
    /// Scan an explicit file or directory path.
    Scan {
        #[arg(long)]
        path: PathBuf,
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
        #[arg(long, conflicts_with_all = ["file_version_id", "media_snapshot_id", "container", "video_codec"])]
        all: bool,
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
    /// List the seeded video encode profiles.
    List,
    /// Show one video encode profile by name.
    Show {
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
    /// List artifact handles, optionally filtered by inspection state.
    List {
        #[arg(long)]
        state: Option<ArtifactStateArg>,
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
