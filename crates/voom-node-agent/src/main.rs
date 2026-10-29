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
    startup(async {
        let args = Args::parse();
        let config = AgentConfig::load(&args.config)?;
        AgentRuntime::new(config)?.run().await
    })
    .await
}

async fn startup<F: std::future::Future>(run: F) -> F::Output {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .json()
        .with_current_span(false)
        .init();
    run.await
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
