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
use tracing_subscriber::EnvFilter;

const VERSION: &str = match option_env!("AVET_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => "dev",
};

fn main() -> Result<()> {
    if std::env::args_os().nth(1).is_some_and(|a| a == "--version" || a == "-V") {
        println!("avet {VERSION}");
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
    let poll_interval = env_u64("POLL_INTERVAL", 60).max(1);

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
                    // A signal during the scan must not start a multi-hour encode.
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

/// Ends the scan loop between jobs. Killed instead, the orphaned encoders keep writing
/// chunk files the restarted instance starts writing too.
fn shutdown_signal() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));

    let mut signals = match Signals::new([SIGTERM, SIGINT, SIGHUP]) {
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

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal())
        .init();
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
    for dir in [input_dir, output_dir] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create directory: {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
