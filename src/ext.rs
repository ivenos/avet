use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Resolves an external CLI tool: sibling of the avet binary first, then PATH.
pub fn external_bin(name: &str) -> OsString {
    let file_name = with_exe_suffix(name);

    if let Some(sibling) = sibling_of_exe(&file_name)
        && is_executable(&sibling)
    {
        return sibling.into_os_string();
    }

    OsString::from(name)
}

/// A neighbour that exists but is not executable must not shadow a working copy on PATH.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn with_exe_suffix(name: &str) -> String {
    let suffix = std::env::consts::EXE_SUFFIX;
    if suffix.is_empty() || name.ends_with(suffix) {
        name.to_string()
    } else {
        format!("{name}{suffix}")
    }
}

fn sibling_of_exe(file_name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(file_name))
}

/// Kills the command if it overruns; nothing else would notice a hung tool.
pub fn output_with_timeout(cmd: &mut Command, secs: u64, what: &str) -> Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run {what}"))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait().with_context(|| format!("wait for {what}"))? {
            Some(status) => {
                return Ok(Output {
                    status,
                    stdout: stdout.join().unwrap_or_default(),
                    stderr: stderr.join().unwrap_or_default(),
                });
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                // Transient: a share that stopped answering or a GPU mid-reset comes back.
                return Err(anyhow::Error::new(crate::job::Transient)
                    .context(format!("{what} did not finish within {secs}s - killed")));
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// `Child::drop` does not wait, so a tool left behind by an early return stays a zombie.
pub fn reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// `args` has to request JSON. A whole-file query needs [`ffprobe_json_with_timeout`].
pub fn ffprobe_json<T: DeserializeOwned>(args: &[&str], input: &Path) -> Result<T> {
    ffprobe_json_with_timeout(args, input, 120)
}

pub fn ffprobe_json_with_timeout<T: DeserializeOwned>(
    args: &[&str],
    input: &Path,
    secs: u64,
) -> Result<T> {
    let mut cmd = Command::new(external_bin("ffprobe"));
    cmd.args(args).arg(input);
    let out = output_with_timeout(&mut cmd, secs, "ffprobe")?;
    if !out.status.success() {
        bail!("ffprobe failed:\n{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    serde_json::from_slice(&out.stdout).context("parse ffprobe json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_that_overruns_is_killed_and_retried_later() {
        let t0 = Instant::now();
        let err = output_with_timeout(Command::new("sleep").arg("30"), 1, "sleep").unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert!(err.downcast_ref::<crate::job::Transient>().is_some(), "got: {err:#}");
    }

    #[test]
    fn a_tool_filling_both_pipes_does_not_block_on_either() {
        let script = "head -c 1000000 /dev/zero; head -c 1000000 /dev/zero >&2";
        let out = output_with_timeout(Command::new("sh").args(["-c", script]), 30, "sh").unwrap();
        assert!(out.status.success());
        assert_eq!((out.stdout.len(), out.stderr.len()), (1_000_000, 1_000_000));
    }
}
