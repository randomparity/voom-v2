//! `voom` CLI entrypoint. Tests live in the sibling `voom_cli` library crate.
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use voom_cli::cli::{Cli, Command};
use voom_cli::commands::{health, init, version};
use voom_cli::envelope::{Local, emit_err};
use voom_cli::logging;
use voom_control_plane::ControlPlane;
use voom_core::{Config, VoomError};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    // Use try_parse so clap errors flow through the JSON envelope writer
    // instead of clap's own stderr exit path — agents reading stdout must
    // never see a non-JSON line.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let kind = e.kind();
            // --help/--version use clap's success-exit path; let it through
            // verbatim because there's no JSON envelope yet for those.
            //
            // DisplayHelpOnMissingArgumentOrSubcommand is deliberately NOT
            // treated as success: invoking `voom` with no subcommand is a
            // malformed call from an agent's perspective (no envelope on
            // stdout, no idea which command ran), so it falls through to the
            // BAD_ARGS arm below and exits 1.
            if matches!(
                kind,
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                e.print().ok();
                return ExitCode::from(0);
            }
            // Everything else is a user error — emit BAD_ARGS envelope.
            let _ = voom_cli::envelope::emit_err(
                "cli",
                "BAD_ARGS",
                e.to_string(),
                Some("Run `voom --help` for usage".into()),
                None,
            );
            return ExitCode::from(1);
        }
    };
    logging::init(&cli.log_level, cli.log_format.to_core());

    let code = match dispatch(cli).await {
        Ok(code) => code,
        Err(err) => {
            // Preserve VoomError codes through anyhow so a user-correctable
            // CONFIG_INVALID isn't collapsed into a generic INTERNAL envelope.
            let code = err
                .downcast_ref::<VoomError>()
                .map_or("INTERNAL", VoomError::code);
            let hint = if code == "INTERNAL" {
                Some("Re-run with --log-level=debug and file a bug".to_owned())
            } else {
                None
            };
            let _ = emit_err("internal", code, err.to_string(), hint, None);
            2
        }
    };
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}

/// Resolve `Config` using the values clap already parsed, so we never re-read
/// `VOOM_LOG_LEVEL` or `VOOM_LOG_FORMAT` from the process environment after a
/// CLI override has won. Otherwise a stale invalid env value (e.g.
/// `VOOM_LOG_FORMAT=xml`) shadowed by `--log-format json` would still fail
/// here as `CONFIG_INVALID` even though the user supplied a valid value.
fn resolve_cfg(cli: &Cli) -> Result<Config, VoomError> {
    Config::resolve(
        cli.database_url.clone(),
        Some(cli.log_level.clone()),
        Some(cli.log_format.as_str().to_owned()),
    )
}

async fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Version => {
            version::run()?;
            Ok(0)
        }
        Command::Health => {
            let cfg = match resolve_cfg(&cli) {
                Ok(cfg) => cfg,
                Err(err) => {
                    voom_cli::envelope::emit_err(
                        "health",
                        err.code(),
                        err.to_string(),
                        None,
                        None,
                    )?;
                    return Ok(2);
                }
            };
            // Build `Local` as soon as config resolves so any subsequent failure
            // (open, probe) emits a properly-attributed `health` envelope.
            let local = Local {
                db_url: cfg.database_url.clone(),
                config_path: cfg.config_path.display().to_string(),
            };
            match ControlPlane::open(&cfg.database_url).await {
                Ok(cp) => Ok(health::run(&cp, local).await?),
                Err(err) => {
                    let hint =
                        (err.code() == "DB_UNREACHABLE").then(|| "Run: voom init".to_owned());
                    voom_cli::envelope::emit_err(
                        "health",
                        err.code(),
                        err.to_string(),
                        hint,
                        Some(local),
                    )?;
                    Ok(2)
                }
            }
        }
        Command::Init => {
            let cfg = match resolve_cfg(&cli) {
                Ok(cfg) => cfg,
                Err(err) => {
                    voom_cli::envelope::emit_err("init", err.code(), err.to_string(), None, None)?;
                    return Ok(2);
                }
            };
            let local = Local {
                db_url: cfg.database_url.clone(),
                config_path: cfg.config_path.display().to_string(),
            };
            Ok(init::run(&cfg.database_url, local).await?)
        }
    }
}
