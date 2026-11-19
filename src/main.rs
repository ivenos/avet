mod audio;
mod av1;
mod config;
mod crop;
mod encode;
mod ext;
mod ffms2;
mod hdr;
mod hevc;
mod interlace;
mod job;
mod mkv;
mod resume;
mod scanner;
mod scene;
#[cfg(test)]
mod schema;
mod subtitle;
mod target_quality;
mod workers;

use anyhow::{Context, Result};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::field::{Field, Visit};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::field::{RecordFields, VisitOutput};
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::{DefaultVisitor, Writer};

const VERSION: &str = match option_env!("AVET_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => "dev",
};

const USAGE: &str = "usage: avet [--version | --help]\nIt watches INPUT_DIR (./input) and writes to OUTPUT_DIR (./output).";

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(reply) = reply(&args)? {
        println!("{reply}");
        return Ok(());
    }
    init_logging();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !hdr::EXPECTED_PANIC.get() {
            default_hook(info);
        }
    }));

    let input_dir = env_path("INPUT_DIR", "./input");
    let output_dir = env_path("OUTPUT_DIR", "./output");
    let poll_interval = env_u64("POLL_INTERVAL", 60).clamp(1, u32::MAX.into());

    tracing::info!(
        version = %VERSION,
        input = %input_dir.display(),
        output = %output_dir.display(),
        poll_s = poll_interval,
        "avet started"
    );

    ensure_dirs(&input_dir, &output_dir)?;

    let ctx = job::JobContext {
        input_dir: input_dir.clone(),
        output_dir: output_dir.clone(),
        poll: Duration::from_secs(poll_interval),
    };

    let shutdown = shutdown_signal();
    let Some(_lock) = lock_output(&output_dir, &shutdown)? else { return Ok(()) };

    loop {
        let scanned = scanner::scan(&input_dir, &output_dir)
            .map(|jobs| jobs.into_iter().filter(|j| job::RETRIES.due(j)).collect::<Vec<_>>());
        match scanned {
            Err(e) => tracing::error!("scanner error: {e:#}"),
            Ok(jobs) if jobs.is_empty() => {
                tracing::debug!("no jobs - sleeping {poll_interval}s");
            }
            Ok(jobs) => {
                tracing::info!("{} job(s) queued", jobs.len());
                for j in &jobs {
                    if shutdown.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let stem = j.stem();
                    if j.delivered {
                        if let Err(e) = job::finish_delivery(j, &ctx) {
                            scanner::report(format!("[{stem}] output is in place, but archiving the source failed: {e:#}"));
                        }
                        continue;
                    }
                    let profile = resume::profile_id(&j.encode_toml);

                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job::run(j, &ctx)))
                        .unwrap_or_else(|panic| Err(anyhow::anyhow!("avet panicked: {}", panic_message(&*panic))));
                    match result {
                        Ok(()) => job::RETRIES.clear(j),
                        Err(e) => job::handle_failure(j, &ctx, stem, &e, shutdown.load(Ordering::Relaxed), profile.as_deref()),
                    }
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("stopping after {stem}");
                        return Ok(());
                    }
                }
            }
        }

        for _ in 0..poll_interval {
            std::thread::sleep(Duration::from_secs(1));
            if shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }
        }
    }
}

fn reply(args: &[std::ffi::OsString]) -> Result<Option<String>> {
    match args {
        [] => Ok(None),
        [a] if a == "--version" || a == "-V" => Ok(Some(format!("avet {VERSION}"))),
        [a] if a == "--help" || a == "-h" => Ok(Some(USAGE.into())),
        _ => {
            let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
            anyhow::bail!("avet takes no argument other than --version or --help, got: {}\n{USAGE}", args.join(" "));
        }
    }
}

/// Ends the scan loop between jobs. Killed instead, the orphaned encoders keep writing
/// chunk files the restarted instance starts writing too.
fn shutdown_signal() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));

    let watched: &[i32] = if hangup_ignored() { &[SIGTERM, SIGINT] } else { &[SIGTERM, SIGINT, SIGHUP] };
    let mut signals = match Signals::new(watched) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("could not install signal handlers ({e}) - avet will keep running on SIGTERM until it is killed");
            return flag;
        }
    };
    let set = Arc::clone(&flag);
    std::thread::spawn(move || {
        // Kept iterating for the process lifetime: dropping `Signals` leaves its own
        // no-op handler installed, and every later signal short of SIGKILL is swallowed.
        for _ in signals.forever() {
            if set.swap(true, Ordering::Relaxed) {
                tracing::warn!("second signal - stopping now, the current file stays unfinished");
                std::process::exit(130);
            }
            job::ATTEMPTS.stop_requested();
            tracing::info!("signal received - finishing the current job, then stopping");
        }
    });

    flag
}

/// A second instance would encode the same files into the same chunks, so it waits.
fn lock_output(output_dir: &std::path::Path, shutdown: &AtomicBool) -> Result<Option<std::fs::File>> {
    let path = output_dir.join(".avet.lock");
    let file = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut announced = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if !announced => {
                tracing::warn!("another avet works on {} - waiting until it stops", output_dir.display());
                announced = true;
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => {
                tracing::warn!("cannot lock {} ({e}) - a second avet on this folder would go unnoticed", path.display());
                return Ok(Some(file));
            }
        }
        for _ in 0..5 {
            std::thread::sleep(Duration::from_secs(1));
            if shutdown.load(Ordering::Relaxed) {
                return Ok(None);
            }
        }
    }
}

pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

fn init_logging() {
    use std::io::IsTerminal;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let name_length = std::env::var("LOG_NAME_LENGTH").ok();
    let max = name_length.as_deref().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal())
        .fmt_fields(ShortNames(max))
        .init();
    if let Some(v) = name_length.filter(|_| max.is_none()) {
        tracing::warn!("LOG_NAME_LENGTH has invalid value {v:?} - logging full names");
    }
}

struct ShortNames(Option<usize>);

impl<'w> FormatFields<'w> for ShortNames {
    fn format_fields<R: RecordFields>(&self, writer: Writer<'w>, fields: R) -> std::fmt::Result {
        let mut visitor = ShortNameVisitor { inner: DefaultVisitor::new(writer, true), max: self.0 };
        fields.record(&mut visitor);
        visitor.inner.finish()
    }
}

struct ShortNameVisitor<'w> {
    inner: DefaultVisitor<'w>,
    max: Option<usize>,
}

impl Visit for ShortNameVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.inner.record_str(field, value)
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.inner.record_error(field, value)
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match self.max.filter(|_| field.name() == "message").and_then(|max| short_name(&format!("{value:?}"), max)) {
            Some(short) => self.inner.record_debug(field, &format_args!("{short}")),
            None => self.inner.record_debug(field, value),
        }
    }
}

fn short_name(message: &str, max: usize) -> Option<String> {
    let rest = message.strip_prefix('[')?;
    let first_line = rest.split('\n').next().unwrap_or(rest);
    let mut depth = 1;
    let (end, _) = first_line.char_indices().find(|&(_, c)| {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        depth == 0
    })?;
    let name = &rest[..end];
    (name.chars().count() > max).then(|| format!("[{}…]{}", name.chars().take(max).collect::<String>(), &rest[end + 1..]))
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(|| default.into(), PathBuf::from)
}

fn env_u64(var: &str, default: u64) -> u64 {
    match std::env::var(var) {
        Err(_) => default,
        Ok(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!("{var} has invalid value {v:?} - using default {default}");
                default
            }
        },
    }
}

fn ensure_dirs(input_dir: &std::path::Path, output_dir: &std::path::Path) -> Result<()> {
    let create = |dir: &std::path::Path| std::fs::create_dir_all(dir).with_context(|| format!("create directory: {}", dir.display()));
    create(input_dir)?;
    if resolved(output_dir)?.starts_with(resolved(input_dir)?) {
        anyhow::bail!(
            "OUTPUT_DIR {} is inside INPUT_DIR {}",
            output_dir.display(), input_dir.display()
        );
    }
    create(output_dir)
}

fn resolved(path: &std::path::Path) -> Result<PathBuf> {
    let existing = path.ancestors().find(|a| a.as_os_str().is_empty() || a.exists()).unwrap_or(path);
    let start = if existing.as_os_str().is_empty() { std::path::Path::new(".") } else { existing };
    let mut real = start.canonicalize().with_context(|| format!("resolve {}", start.display()))?;
    for part in path.strip_prefix(existing).unwrap_or(path).components() {
        match part {
            std::path::Component::ParentDir => { real.pop(); }
            std::path::Component::Normal(name) => real.push(name),
            _ => {}
        }
    }
    Ok(real)
}

/// `nohup` starts a program with SIGHUP ignored, as the SigIgn mask in /proc shows.
fn hangup_ignored() -> bool {
    std::fs::read_to_string("/proc/self/status").ok()
        .and_then(|status| status.lines().find_map(|l| l.strip_prefix("SigIgn:").map(|m| m.trim().to_owned())))
        .and_then(|mask| u64::from_str_radix(&mask, 16).ok())
        .is_some_and(|mask| mask & (1 << (SIGHUP - 1)) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_output_folder_inside_the_input_folder_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let input = dir.path().join("input");
        assert!(ensure_dirs(&input, &dir.path().join("output")).is_ok());
        assert!(ensure_dirs(&input, &input.join("movies/out")).is_err());
        assert!(ensure_dirs(&input, &input).is_err());
        assert!(ensure_dirs(&input, &dir.path().join("input-out")).is_ok());
    }

    #[test]
    fn a_refused_output_folder_is_not_created_and_links_and_dots_are_followed() {
        let dir = tempfile::TempDir::new().unwrap();
        let input = dir.path().join("input");
        assert!(ensure_dirs(&input, &input.join("movies/out")).is_err());
        assert!(!input.join("movies").exists());

        std::os::unix::fs::symlink(&input, dir.path().join("link")).unwrap();
        assert!(ensure_dirs(&input, &dir.path().join("link/out")).is_err());
        assert!(ensure_dirs(&input, &input.join("new/../../out")).is_ok());
        assert!(dir.path().join("out").is_dir());
    }

    #[test]
    fn log_name_length_cuts_only_the_name_in_front_of_a_job_line() {
        assert_eq!(short_name("[Dune.Part.Two.2024.2160p.UHD.BluRay] encoding: 4 chunks", 10).as_deref(), Some("[Dune.Part.…] encoding: 4 chunks"));
        assert_eq!(short_name("[Amélie.2001.1080p] done", 6).as_deref(), Some("[Amélie…] done"));
        assert_eq!(short_name("[[SubsPlease] Show - 01 [A1B2]] done", 12).as_deref(), Some("[[SubsPlease]…] done"));
        assert_eq!(short_name("[Short] done", 10), None);
        assert_eq!(short_name("avet started", 3), None);
        assert_eq!(short_name("[never closed", 3), None);
        assert_eq!(short_name("[Show [1080p] job failed\nparse encode.toml\n  |  ]", 4), None);
    }

    #[test]
    fn an_argument_that_is_not_utf8_is_refused_like_any_unknown_one() {
        use std::os::unix::ffi::OsStringExt;
        let bad = std::ffi::OsString::from_vec(vec![0xff]);
        assert!(reply(&[bad.clone()]).is_err());
        assert!(reply(&[bad, "--bogus".into()]).is_err());
        assert!(reply(&["--bogus".into()]).is_err());
        assert!(reply(&["--version".into(), "--version".into()]).is_err());
        assert_eq!(reply(&["-V".into()]).unwrap(), Some(format!("avet {VERSION}")));
        assert_eq!(reply(&["--help".into()]).unwrap().as_deref(), Some(USAGE));
        assert_eq!(reply(&[]).unwrap(), None);
    }

    #[test]
    fn a_second_instance_on_the_same_output_waits_and_stops_on_a_signal() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = lock_output(dir.path(), &AtomicBool::new(false)).unwrap();
        assert!(first.is_some());
        assert!(lock_output(dir.path(), &AtomicBool::new(true)).unwrap().is_none());
        drop(first);
        assert!(lock_output(dir.path(), &AtomicBool::new(true)).unwrap().is_some());
    }
}
