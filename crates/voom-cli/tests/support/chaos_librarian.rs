#![allow(
    dead_code,
    reason = "E2E support helpers are introduced before every test uses them"
)]

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

#[derive(Debug, Clone)]
pub struct ChaosLibrarian {
    pub workspace_root: PathBuf,
    pub submodule_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ChaosReadiness {
    pub revision: String,
    pub capabilities: Value,
}

#[derive(Debug)]
pub struct ChaosRun {
    pub _tmp: TempDir,
    pub run_dir: PathBuf,
    pub report: Value,
}

impl ChaosLibrarian {
    pub fn discover() -> Result<Self, Box<dyn std::error::Error>> {
        let workspace_root = workspace_root();
        let submodule_dir = workspace_root.join("third_party/chaos-librarian");
        if !submodule_dir.join("pyproject.toml").is_file() {
            return Err(io::Error::other(format!(
                "Chaos Librarian submodule is not initialized at {}",
                submodule_dir.display()
            ))
            .into());
        }
        Ok(Self {
            workspace_root,
            submodule_dir,
        })
    }

    pub fn validate_ready(&self) -> Result<ChaosReadiness, Box<dyn std::error::Error>> {
        let status = command_output(Command::new("git").current_dir(&self.workspace_root).args([
            "submodule",
            "status",
            "third_party/chaos-librarian",
        ]))?;
        let line = String::from_utf8(status.stdout)?;
        let trimmed = line.trim_end();
        if !trimmed.starts_with(' ') {
            return Err(io::Error::other(format!(
                "submodule must be clean and initialized: {trimmed}"
            ))
            .into());
        }
        let revision = trimmed
            .split_whitespace()
            .next()
            .ok_or_else(|| io::Error::other("missing submodule revision"))?
            .to_owned();

        command_output(
            Command::new("uv")
                .current_dir(&self.submodule_dir)
                .args(["sync", "--locked"]),
        )?;
        let capabilities = self.uv_json(["run", "chaos-librarian", "capabilities", "--json"])?;
        Ok(ChaosReadiness {
            revision,
            capabilities,
        })
    }

    pub fn materialize(&self, scenario: &Path) -> Result<ChaosRun, Box<dyn std::error::Error>> {
        let tmp = TempDir::new()?;
        let run_dir = tmp.path().join("run");
        let report = self.uv_json_with_args([
            "run",
            "chaos-librarian",
            "materialize",
            scenario
                .to_str()
                .ok_or_else(|| io::Error::other("scenario path is not UTF-8"))?,
            "--out",
            run_dir
                .to_str()
                .ok_or_else(|| io::Error::other("run dir path is not UTF-8"))?,
            "--json",
        ])?;
        Ok(ChaosRun {
            _tmp: tmp,
            run_dir,
            report,
        })
    }

    pub fn step_next(&self, run_dir: &Path) -> Result<Value, Box<dyn std::error::Error>> {
        self.uv_json_with_args([
            "run",
            "chaos-librarian",
            "step",
            run_dir
                .to_str()
                .ok_or_else(|| io::Error::other("run dir path is not UTF-8"))?,
            "--next",
            "1",
            "--json",
        ])
    }

    pub fn compare_final_state(
        &self,
        run_dir: &Path,
        observed_state: &Path,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let output = Command::new("uv")
            .current_dir(&self.submodule_dir)
            .args([
                "run",
                "chaos-librarian",
                "compare",
                run_dir
                    .to_str()
                    .ok_or_else(|| io::Error::other("run dir path is not UTF-8"))?,
                observed_state
                    .to_str()
                    .ok_or_else(|| io::Error::other("observed-state path is not UTF-8"))?,
                "--mode",
                "final-state",
                "--json",
            ])
            .output()?;
        if output.status.code() != Some(0) {
            return Err(io::Error::other(format!(
                "chaos-librarian compare failed with {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    pub fn upstream_scenario(&self, name: &str) -> PathBuf {
        self.submodule_dir
            .join("tests/fixtures/scenarios")
            .join(name)
    }

    pub fn voom_scenario(&self, name: &str) -> PathBuf {
        self.workspace_root
            .join("crates/voom-cli/tests/fixtures/chaos")
            .join(name)
    }

    fn uv_json<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.uv_json_with_args(args)
    }

    fn uv_json_with_args<I, S>(&self, args: I) -> Result<Value, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = command_output(
            Command::new("uv")
                .current_dir(&self.submodule_dir)
                .args(args),
        )?;
        Ok(serde_json::from_slice(&output.stdout)?)
    }
}

fn command_output(command: &mut Command) -> Result<Output, Box<dyn std::error::Error>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(output);
    }
    Err(io::Error::other(format!(
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
    .into())
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}
