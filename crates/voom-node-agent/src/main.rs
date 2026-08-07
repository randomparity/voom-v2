use std::path::PathBuf;

use clap::Parser;
use voom_core::VoomError;
use voom_node_agent::config::AgentConfig;
use voom_node_agent::runtime::AgentRuntime;

#[derive(Debug, Parser)]
#[command(version, about = "Supervise pull-based workers for one Voom node")]
struct Args {
    /// Strict node-agent TOML configuration.
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), VoomError> {
    let args = Args::parse();
    let config = AgentConfig::load(&args.config)?;
    AgentRuntime::new(config)?.run().await
}
