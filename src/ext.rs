use anyhow::{Context, Result};
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

/// A neighbor that exists but is not executable must not shadow a working copy on PATH.
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
                // Not a plain wait(): a child wedged in uninterruptible I/O on a dead
                // share never reaps, and waiting on it hangs the daemon for good.
                let reaped = wait_briefly(&mut child);
                if !reaped {
                    tracing::warn!("{what} did not react to the kill - leaving it behind");
                }
                // Transient: a share that stopped answering or a GPU mid-reset comes back.
                return Err(anyhow::Error::new(crate::job::Transient).context(format!(
                    "{what} did not finish within {secs}s - killed{}",
                    last_output(stderr)
                )));
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// What the tool said before it wedged, as a suffix for the timeout error. Polled rather
/// than joined: a grandchild holding the pipe open would block the join forever.
fn last_output(handle: JoinHandle<Vec<u8>>) -> String {
    const LINES: usize = 15;

    let deadline = Instant::now() + Duration::from_secs(2);
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !handle.is_finished() {
        return String::new();
    }
    let raw = handle.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&raw);
    let text = text.trim_end();
    if text.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = text.lines().rev().take(LINES).collect();
    format!(":\n{}", tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

fn wait_briefly(child: &mut std::process::Child) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    false
}

/// The error for a tool that ran and failed. A tool stopped from outside (the OOM killer,
/// Ctrl-C) and a full disk both clear on their own; a tool that crashed does not.
pub fn tool_error(what: &str, status: std::process::ExitStatus, message: &str) -> anyhow::Error {
    let err = anyhow::anyhow!("{what} failed:\n{}", message.trim());
    if stopped_from_outside(status) || out_of_space(message) {
        err.context(crate::job::Transient)
    } else {
        err
    }
}

fn stopped_from_outside(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    matches!(status.signal(), Some(SIGHUP | SIGINT | SIGKILL | SIGTERM))
}

fn out_of_space(message: &str) -> bool {
    ["No space left on device", "ENOSPC", "Disk quota exceeded", "EDQUOT"]
        .iter()
        .any(|m| message.contains(m))
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

pub fn drain_text<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
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
        return Err(tool_error("ffprobe", out.status, &String::from_utf8_lossy(&out.stderr)));
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
    fn what_a_wedged_tool_said_before_the_kill_is_in_the_error() {
        // Without it the operator is left with the timeout alone and no idea what it hung on.
        let err = output_with_timeout(
            Command::new("sh").args(["-c", "echo 'Cannot open display' >&2; exec sleep 30"]),
            1,
            "FFVship",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("Cannot open display"), "got: {err:#}");
    }

    #[test]
    fn a_signalled_tool_is_retried_even_under_a_caller_context() {
        use anyhow::Context as _;

        // How the OOM killer and a Ctrl-C to the process group arrive.
        let out = output_with_timeout(Command::new("sh").args(["-c", "kill -TERM $$"]), 30, "sh").unwrap();
        assert_eq!(out.status.code(), None, "the shell was not signalled");

        let err = tool_error("mkvmerge", out.status, "");
        assert!(err.downcast_ref::<crate::job::Transient>().is_some(), "got: {err:#}");

        // Callers add their own step name on top, and that must not hide it.
        let wrapped = Err::<(), _>(err).context("mux the final file").unwrap_err();
        assert!(wrapped.downcast_ref::<crate::job::Transient>().is_some(), "got: {wrapped:#}");
    }

    #[test]
    fn a_crashed_tool_is_not_retried_forever() {
        use std::os::unix::process::ExitStatusExt;

        for signal in [6, 11] {
            let err = tool_error("SvtAv1EncApp", std::process::ExitStatus::from_raw(signal), "");
            assert!(err.downcast_ref::<crate::job::Transient>().is_none(), "signal {signal}: {err:#}");
        }
        let killed = tool_error("SvtAv1EncApp", std::process::ExitStatus::from_raw(9), "");
        assert!(killed.downcast_ref::<crate::job::Transient>().is_some(), "got: {killed:#}");
    }

    #[test]
    fn a_full_disk_is_retried_and_an_ordinary_failure_is_not() {
        let out = output_with_timeout(Command::new("sh").args(["-c", "exit 1"]), 30, "sh").unwrap();

        let full = tool_error("ffmpeg", out.status, "av_interleaved_write_frame(): No space left on device");
        assert!(full.downcast_ref::<crate::job::Transient>().is_some(), "got: {full:#}");
        let quota = tool_error("mkvmerge", out.status, "Error: Disk quota exceeded");
        assert!(quota.downcast_ref::<crate::job::Transient>().is_some(), "got: {quota:#}");

        let broken = tool_error("ffmpeg", out.status, "Invalid data found when processing input");
        assert!(broken.downcast_ref::<crate::job::Transient>().is_none(), "got: {broken:#}");
    }

    #[test]
    fn a_stderr_with_a_byte_in_another_encoding_is_kept_rather_than_dropped() {
        let mut child = Command::new("sh")
            .args(["-c", "printf 'cannot open Filme_\\344.mkv' >&2"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let text = drain_text(child.stderr.take()).join().unwrap();
        child.wait().unwrap();
        assert_eq!(text, "cannot open Filme_\u{FFFD}.mkv");
    }

    #[test]
    fn a_tool_filling_both_pipes_does_not_block_on_either() {
        let script = "head -c 1000000 /dev/zero; head -c 1000000 /dev/zero >&2";
        let out = output_with_timeout(Command::new("sh").args(["-c", script]), 30, "sh").unwrap();
        assert!(out.status.success());
        assert_eq!((out.stdout.len(), out.stderr.len()), (1_000_000, 1_000_000));
    }
}
