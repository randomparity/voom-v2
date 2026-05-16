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
