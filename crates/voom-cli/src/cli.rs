use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "voom", about = "VOOM control plane CLI", long_about = None)]
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
    /// Register and manage execution nodes.
    #[command(subcommand)]
    Node(NodeCommand),
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

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum ComplianceCommand {
    /// Generate a compliance report from durable policy and input rows.
    Report {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
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
