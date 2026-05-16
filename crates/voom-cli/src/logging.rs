use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::{EnvFilter, fmt};
use voom_core::LogFormat;

/// Install the global tracing subscriber. Writes to stderr so it never collides
/// with the envelope on stdout.
pub fn init(level: &str, format: LogFormat) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let writer = std::io::stderr.with_max_level(tracing::Level::TRACE);

    match format {
        LogFormat::Json => {
            fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .json()
                .with_current_span(false)
                .init();
        }
        LogFormat::Text => {
            fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_target(false)
                .init();
        }
    }
}
