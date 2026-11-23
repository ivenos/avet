use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DISK_FULL: [&str; 4] = ["No space left", "ENOSPC", "Disk quota exceeded", "EDQUOT"];

static DISK_FULL_ARGS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

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

pub fn spawn(cmd: &mut Command, what: &str) -> Result<Child> {
    note_disk_full_args(cmd);
    cmd.spawn().with_context(|| format!("start {what}")).context(crate::job::Transient)
}

pub fn output_with_timeout(cmd: &mut Command, secs: u64, what: &str) -> Result<Output> {
    let mut child = spawn(cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()), what)?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait().with_context(|| format!("wait for {what}"))? {
            Some(status) => {
                let out = Output {
                    status,
                    stdout: stdout.join().unwrap_or_default(),
                    stderr: stderr.join().unwrap_or_default(),
                };
                if ffmpeg_stopped(what, status) {
                    return Err(tool_error(what, status, &String::from_utf8_lossy(&out.stderr)));
                }
                return Ok(out);
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

pub fn whole_file_timeout(path: &Path, base: u64) -> u64 {
    base + std::fs::metadata(path).map_or(0, |m| m.len() / 10_000_000)
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
    format!(":\n{}", tail(text, LINES))
}

pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.trim().lines().collect();
    let skipped = all.len().saturating_sub(lines);
    let kept = all[skipped..].join("\n");
    if skipped == 0 { kept } else { format!("[{skipped} earlier lines]\n{kept}") }
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

pub fn exit_within(child: &mut std::process::Child, wait: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => return None,
        }
    }
}

/// The error for a tool that ran and failed. A tool stopped from outside (the OOM killer,
/// Ctrl-C) and a full disk both clear on their own; a tool that crashed does not.
pub fn tool_error(what: &str, status: std::process::ExitStatus, message: &str) -> anyhow::Error {
    let err = anyhow::anyhow!("{what} failed ({status}):\n{}", tail(message, 40));
    if stopped_from_outside(what, status) {
        err.context(crate::job::Killed).context(crate::job::Transient)
    } else if out_of_space(message) {
        err.context(crate::job::Transient)
    } else {
        err
    }
}

pub fn stopped_from_outside(what: &str, status: std::process::ExitStatus) -> bool {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGKILL, SIGTERM};
    use std::os::unix::process::ExitStatusExt;
    matches!(status.signal(), Some(SIGHUP | SIGINT | SIGKILL | SIGTERM)) || ffmpeg_stopped(what, status)
}

/// ffmpeg catches SIGTERM and SIGINT and exits with 255.
fn ffmpeg_stopped(what: &str, status: std::process::ExitStatus) -> bool {
    what.starts_with("ffmpeg") && status.code() == Some(255)
}

fn note_disk_full_args(cmd: &Command) {
    let mut noted = DISK_FULL_ARGS.lock().unwrap_or_else(PoisonError::into_inner);
    let paths = cmd.get_args().map(|a| a.to_string_lossy()).filter(|a| a.contains('/'));
    for arg in paths.filter(|a| DISK_FULL.iter().any(|m| a.contains(m))) {
        // mkvmerge's JSON output carries file names escaped.
        if let Ok(json) = serde_json::to_string(arg.as_ref()) {
            noted.insert(json[1..json.len() - 1].to_string());
        }
        // av_log prints these control bytes as '?'.
        noted.insert(arg.replace(|c: char| matches!(c, '\0'..='\x07' | '\x0e'..='\x1f'), "?"));
        noted.insert(arg.into_owned());
    }
}

/// mkvmerge words it "No space left to write to".
fn out_of_space(message: &str) -> bool {
    let noted = DISK_FULL_ARGS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut longest_first: Vec<&String> = noted.iter().collect();
    longest_first.sort_by_key(|arg| std::cmp::Reverse(arg.len()));
    let said = longest_first.iter().fold(message.to_string(), |text, arg| text.replace(arg.as_str(), ""));
    // ffmpeg indents the source's metadata.
    said.lines().filter(|l| !l.starts_with(char::is_whitespace)).any(|l| DISK_FULL.iter().any(|m| l.contains(m)))
}

/// mkvmerge otherwise words its messages in the user's language, and under a locale without UTF-8 drops the non-ASCII bytes of its arguments.
pub fn mkvmerge() -> Command {
    let mut cmd = Command::new(external_bin("mkvmerge"));
    cmd.env("LC_ALL", "C.UTF-8").args(["--ui-language", "en_US"]);
    cmd
}

pub fn mkvmerge_output(cmd: &mut Command, secs: u64, what: &str) -> Result<Output> {
    let out = output_with_timeout(cmd, secs, what)?;
    if out.status.code().unwrap_or(2) >= 2 {
        return Err(tool_error(what, out.status, &String::from_utf8_lossy(&out.stdout)));
    }
    Ok(out)
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
        let err = output_with_timeout(
            Command::new("sh").args(["-c", "echo 'Cannot open display' >&2; exec sleep 30"]),
            1,
            "FFVship",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("Cannot open display"), "got: {err:#}");
    }

    #[test]
    fn a_signaled_tool_is_retried_even_under_a_caller_context() {
        use anyhow::Context as _;

        // How the OOM killer and a Ctrl-C to the process group arrive.
        let out = output_with_timeout(Command::new("sh").args(["-c", "kill -TERM $$"]), 30, "sh").unwrap();
        assert_eq!(out.status.code(), None, "the shell was not signaled");

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
        assert!(killed.context("chunk 00007").downcast_ref::<crate::job::Killed>().is_some());
    }

    #[test]
    fn a_tool_that_dies_without_a_word_is_reported_with_its_exit_status() {
        let out = output_with_timeout(Command::new("sh").args(["-c", "kill -SEGV $$"]), 30, "sh").unwrap();
        let crashed = format!("{:#}", tool_error("FFVship", out.status, ""));
        assert!(crashed.starts_with("FFVship failed (signal: 11 (SIGSEGV)"), "got: {crashed}");

        let out = output_with_timeout(Command::new("sh").args(["-c", "exit 3"]), 30, "sh").unwrap();
        let exited = format!("{:#}", tool_error("ffmpeg", out.status, ""));
        assert!(exited.starts_with("ffmpeg failed (exit status: 3)"), "got: {exited}");
    }

    #[test]
    fn a_full_disk_is_retried_and_an_ordinary_failure_is_not() {
        let out = output_with_timeout(Command::new("sh").args(["-c", "exit 1"]), 30, "sh").unwrap();

        let full = tool_error("ffmpeg", out.status, "av_interleaved_write_frame(): No space left on device");
        assert!(full.downcast_ref::<crate::job::Transient>().is_some(), "got: {full:#}");
        assert!(full.downcast_ref::<crate::job::Killed>().is_none(), "got: {full:#}");
        let quota = tool_error("mkvmerge", out.status, "Error: Disk quota exceeded");
        assert!(quota.downcast_ref::<crate::job::Transient>().is_some(), "got: {quota:#}");
        let mkvmerge = tool_error("mkvmerge", out.status, "Error: An exception occurred when writing the \
            destination file. The drive may be full. Exception details: reading from/writing to the file \
            error; No space left to write to");
        assert!(mkvmerge.downcast_ref::<crate::job::Transient>().is_some(), "got: {mkvmerge:#}");

        let broken = tool_error("ffmpeg", out.status, "Invalid data found when processing input");
        assert!(broken.downcast_ref::<crate::job::Transient>().is_none(), "got: {broken:#}");
    }

    #[test]
    fn what_each_tool_prints_on_a_full_disk_is_retried() {
        let out = output_with_timeout(Command::new("sh").args(["-c", "exit 1"]), 30, "sh").unwrap();
        for message in [
            "[out#0/matroska @ 0x5633cf30ea00] Error writing trailer: No space left on device",
            "[out#0/matroska @ 0x564b92549a00] Error opening output /inodes/out.mkv: No space left on device\nError opening output file /inodes/out.mkv.\nError opening output files: No space left on device",
            "Writing index... Error writing index: Failed to write to '/blocks/out.ffindex': No space left on device",
            "Error: An exception occurred when writing the destination file. The drive may be full. Exception details: reading from/writing to the file error; No space left to write to",
        ] {
            let err = tool_error("tool", out.status, message);
            assert!(err.downcast_ref::<crate::job::Transient>().is_some(), "{message}");
        }
    }

    #[test]
    fn a_title_tag_worded_like_a_full_disk_is_not_one() {
        let out = output_with_timeout(Command::new("sh").args(["-c", "exit 1"]), 30, "sh").unwrap();
        let tags = "Input #0, matroska,webm, from 'x.mkv':\n  Metadata:\n    title           : No space left on device\nError: invalid data";
        assert!(tool_error("tool", out.status, tags).downcast_ref::<crate::job::Transient>().is_none());
    }

    #[test]
    fn a_file_named_like_a_full_disk_is_not_one() {
        for (name, tool_says) in [
            ("/input/p/No space left on device.mkv", "Error opening input file /input/p/No space left on device.mkv."),
            ("/input/p/x: Disk quota exceeded\n.mkv", "Error opening input file /input/p/x: Disk quota exceeded\n.mkv."),
            ("/input/p/ENOSPC \"1\".mkv", r#"{"file_name": "/input/p/ENOSPC \"1\".mkv", "errors": ["unreadable"]}"#),
            ("/input/p/No space left\x01 on device.mkv", "Error opening input file /input/p/No space left? on device.mkv."),
        ] {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "exit 1", name]);
            let out = output_with_timeout(&mut cmd, 30, "sh").unwrap();

            let broken = tool_error("ffmpeg", out.status, tool_says);
            assert!(broken.downcast_ref::<crate::job::Transient>().is_none(), "{tool_says}");
            let full = tool_error("ffmpeg", out.status, &format!("{tool_says}\nError opening output {name}: No space left on device"));
            assert!(full.downcast_ref::<crate::job::Transient>().is_some(), "{tool_says}");
        }
    }

    #[test]
    fn a_path_inside_another_noted_path_leaves_nothing_of_it_behind() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 1", "/output/.avet_No space left", "/output/.avet_No space left/No space left.mkv"]);
        let out = output_with_timeout(&mut cmd, 30, "sh").unwrap();
        let broken = tool_error("ffmpeg", out.status, "Error opening input file /output/.avet_No space left/No space left.mkv.");
        assert!(broken.downcast_ref::<crate::job::Transient>().is_none(), "got: {broken:#}");
    }

    #[test]
    fn a_title_worded_like_a_full_disk_does_not_hide_a_real_one() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 1", "Disk quota exceeded"]);
        let out = output_with_timeout(&mut cmd, 30, "sh").unwrap();
        let full = tool_error("mkvmerge", out.status, "Error: Disk quota exceeded");
        assert!(full.downcast_ref::<crate::job::Transient>().is_some(), "got: {full:#}");
    }

    #[test]
    fn ffmpeg_stopped_from_outside_counts_as_a_kill_and_its_own_errors_do_not() {
        use std::os::unix::process::ExitStatusExt;
        let exit = |code: i32| std::process::ExitStatus::from_raw(code << 8);

        for what in ["ffmpeg track extraction", "ffmpeg filter (chunk 00001)", "ffmpeg remux"] {
            let stopped = tool_error(what, exit(255), "");
            assert!(stopped.downcast_ref::<crate::job::Transient>().is_some(), "{what}");
            assert!(stopped.downcast_ref::<crate::job::Killed>().is_some(), "{what}");
        }
        for code in [1, 8, 183, 234, 254] {
            let broken = tool_error("ffmpeg", exit(code), "");
            assert!(broken.downcast_ref::<crate::job::Transient>().is_none(), "exit {code}");
        }
        for what in ["mkvmerge", "encoder (chunk 00001)", "FFVship", "ffmsindex"] {
            let other = tool_error(what, exit(255), "");
            assert!(other.downcast_ref::<crate::job::Transient>().is_none(), "{what}");
        }
    }

    #[test]
    fn a_caller_that_reads_only_the_exit_status_still_sees_ffmpeg_stopped() {
        let stopped = output_with_timeout(Command::new("sh").args(["-c", "exit 255"]), 30, "ffmpeg copy check").unwrap_err();
        assert!(stopped.downcast_ref::<crate::job::Killed>().is_some(), "got: {stopped:#}");

        let refused = output_with_timeout(Command::new("sh").args(["-c", "exit 234"]), 30, "ffmpeg copy check").unwrap();
        assert_eq!(refused.status.code(), Some(234));
        let other = output_with_timeout(Command::new("sh").args(["-c", "exit 255"]), 30, "mkvmerge").unwrap();
        assert_eq!(other.status.code(), Some(255));
    }

    #[test]
    fn a_tool_that_complains_about_every_packet_is_cut_to_its_last_lines() {
        let out = output_with_timeout(Command::new("sh").args(["-c", "exit 1"]), 30, "sh").unwrap();
        let noise: String = (1..=5000).map(|i| format!("corrupt packet {i}\n")).collect();
        let err = format!("{:#}", tool_error("ffmpeg", out.status, &noise));
        assert!(err.contains("[4960 earlier lines]") && err.ends_with("corrupt packet 5000"), "got: {err}");
        assert_eq!(err.lines().count(), 42);
        assert_eq!(tail("one\ntwo\n", 40), "one\ntwo");
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
