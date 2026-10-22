use std::io;
use std::path::Path;

/// Writes an executable fixture without opening its inode for writing in the test process.
///
/// Unix test processes can fork concurrently. Writing in a child prevents a sibling
/// fork from inheriting a writable descriptor and causing ETXTBSY at fixture exec.
/// The caller owns the destination and its temporary directory. Non-Unix callers
/// retain ordinary file-writing behavior; shell fixtures are Unix-only.
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let mut child = Command::new("/bin/sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "fixture-writer"])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        let write_result = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(contents.as_ref()),
            None => Err(io::Error::other("executable fixture writer has no stdin")),
        };
        let status = child.wait()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "executable fixture writer failed for {} ({status})",
                path.display()
            )));
        }
        write_result
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

#[cfg(test)]
#[path = "executable_test.rs"]
mod tests;
