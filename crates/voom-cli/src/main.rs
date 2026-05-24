//! `voom` CLI entrypoint. Tests live in the sibling `voom_cli` library crate.
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use voom_cli::cli::{Cli, Command, ComplianceCommand, NodeCommand, PlanCommand, WorkerCommand};
use voom_cli::commands::{compliance, health, init, node, plan, version, worker};
use voom_cli::envelope::{Local, emit_err};
use voom_cli::logging;
use voom_control_plane::HealthPlane;
use voom_core::{Config, ErrorCode, VoomError};

/// Process exit codes used by the `voom` binary. The numeric values are
/// public contract: agents key on these.
///
/// Replaces the previous `Result<i32>` signature on `dispatch`, where
/// `u8::try_from(code).unwrap_or(2)` would silently clamp any future
/// out-of-range code to 2 — hiding the real exit.
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
enum Exit {
    Ok = 0,
    BadArgs = 1,
    Failure = 2,
}

impl Exit {
    /// Map an integer code returned by `health::run` / `init::run` (which
    /// keep `i32` for test ergonomics) into the typed exit set. Anything
    /// outside `{0, 1}` becomes `Failure` rather than being silently
    /// clamped to 2 — this is the explicit decision the old `unwrap_or(2)`
    /// hid.
    fn from_run_code(code: i32) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::BadArgs,
            _ => Self::Failure,
        }
    }
}

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
                return ExitCode::from(Exit::Ok as u8);
            }
            // Everything else is a user error — emit BAD_ARGS envelope.
            let _ = voom_cli::envelope::emit_err(
                "cli",
                ErrorCode::BadArgs.as_str(),
                e.to_string(),
                Some("Run `voom --help` for usage".into()),
                None,
            );
            return ExitCode::from(Exit::BadArgs as u8);
        }
    };
    logging::init(&cli.log_level, cli.log_format.to_core());

    let exit = match dispatch(cli).await {
        Ok(exit) => exit,
        Err(err) => {
            // Preserve VoomError codes through anyhow so a user-correctable
            // CONFIG_INVALID isn't collapsed into a generic INTERNAL envelope.
            let error_code = err
                .downcast_ref::<VoomError>()
                .map_or(ErrorCode::Internal, VoomError::error_code);
            let hint = if matches!(error_code, ErrorCode::Internal) {
                Some("Re-run with --log-level=debug and file a bug".to_owned())
            } else {
                None
            };
            let _ = emit_err("internal", error_code.as_str(), err.to_string(), hint, None);
            Exit::Failure
        }
    };
    ExitCode::from(exit as u8)
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

async fn dispatch(cli: Cli) -> Result<Exit> {
    match cli.command {
        Command::Version => {
            version::run()?;
            Ok(Exit::Ok)
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
                    return Ok(Exit::Failure);
                }
            };
            // Build `Local` as soon as config resolves so any subsequent failure
            // (open, probe) emits a properly-attributed `health` envelope.
            let local = Local {
                db_url: cfg.database_url.clone(),
                config_path: cfg.config_path.display().to_string(),
            };
            match HealthPlane::open(&cfg.database_url).await {
                Ok(hp) => Ok(Exit::from_run_code(health::run(&hp, local).await?)),
                Err(err) => {
                    // Share the hint mapper with `health::run` so the two
                    // open-failure paths cannot give different operator
                    // guidance for the same error code.
                    let hint = health::voom_error_hint(&err);
                    voom_cli::envelope::emit_err(
                        "health",
                        err.code(),
                        err.to_string(),
                        hint,
                        Some(local),
                    )?;
                    Ok(Exit::Failure)
                }
            }
        }
        Command::Init => {
            let cfg = match resolve_cfg(&cli) {
                Ok(cfg) => cfg,
                Err(err) => {
                    voom_cli::envelope::emit_err("init", err.code(), err.to_string(), None, None)?;
                    return Ok(Exit::Failure);
                }
            };
            let local = Local {
                db_url: cfg.database_url.clone(),
                config_path: cfg.config_path.display().to_string(),
            };
            Ok(Exit::from_run_code(
                init::run(&cfg.database_url, local).await?,
            ))
        }
        Command::Plan(PlanCommand::DryRun {
            policy_file,
            input_fixture,
        }) => Ok(Exit::from_run_code(
            plan::dry_run(&policy_file, &input_fixture).await?,
        )),
        Command::Plan(PlanCommand::Show {
            policy_version_id,
            input_set_id,
        }) => {
            let cfg = match resolve_cfg(&cli) {
                Ok(cfg) => cfg,
                Err(err) => {
                    voom_cli::envelope::emit_err("plan", err.code(), err.to_string(), None, None)?;
                    return Ok(Exit::Failure);
                }
            };
            let local = Local {
                db_url: cfg.database_url.clone(),
                config_path: cfg.config_path.display().to_string(),
            };
            Ok(Exit::from_run_code(
                plan::show(&cfg.database_url, local, policy_version_id, input_set_id).await?,
            ))
        }
        Command::Compliance(command) => dispatch_compliance(&cli, command).await,
        Command::Node(ref command) => dispatch_node(&cli, command.clone()).await,
        Command::Worker(ref command) => dispatch_worker(&cli, command.clone()).await,
    }
}

async fn dispatch_node(cli: &Cli, command: NodeCommand) -> Result<Exit> {
    let cfg = match resolve_cfg(cli) {
        Ok(cfg) => cfg,
        Err(err) => {
            voom_cli::envelope::emit_err("node", err.code(), err.to_string(), None, None)?;
            return Ok(Exit::Failure);
        }
    };
    let local = Local {
        db_url: cfg.database_url.clone(),
        config_path: cfg.config_path.display().to_string(),
    };
    Ok(Exit::from_run_code(
        node::run(&cfg.database_url, local, command).await?,
    ))
}

async fn dispatch_worker(cli: &Cli, command: WorkerCommand) -> Result<Exit> {
    let cfg = match resolve_cfg(cli) {
        Ok(cfg) => cfg,
        Err(err) => {
            voom_cli::envelope::emit_err("worker", err.code(), err.to_string(), None, None)?;
            return Ok(Exit::Failure);
        }
    };
    let local = Local {
        db_url: cfg.database_url.clone(),
        config_path: cfg.config_path.display().to_string(),
    };
    Ok(Exit::from_run_code(
        worker::run(&cfg.database_url, local, command).await?,
    ))
}

async fn dispatch_compliance(cli: &Cli, command: ComplianceCommand) -> Result<Exit> {
    let cfg = match resolve_cfg(cli) {
        Ok(cfg) => cfg,
        Err(err) => {
            voom_cli::envelope::emit_err("compliance", err.code(), err.to_string(), None, None)?;
            return Ok(Exit::Failure);
        }
    };
    let local = Local {
        db_url: cfg.database_url.clone(),
        config_path: cfg.config_path.display().to_string(),
    };
    let code = match command {
        ComplianceCommand::Report {
            policy_version_id,
            input_set_id,
        } => compliance::report(&cfg.database_url, local, policy_version_id, input_set_id).await?,
        ComplianceCommand::Apply {
            policy_version_id,
            input_set_id,
        } => compliance::apply(&cfg.database_url, local, policy_version_id, input_set_id).await?,
        ComplianceCommand::Execute {
            policy_version_id,
            input_set_id,
        } => compliance::execute(&cfg.database_url, local, policy_version_id, input_set_id).await?,
    };
    Ok(Exit::from_run_code(code))
}
