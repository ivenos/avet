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
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
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
        input = %input_dir.display(),
        output = %output_dir.display(),
        poll_s = poll_interval,
        "avet started"
    );

    ensure_dirs(&input_dir, &output_dir)?;

    let ctx = job::JobContext {
        input_dir: input_dir.clone(),
        output_dir: output_dir.clone(),
    };

    let shutdown = shutdown_signal();

    loop {
        match scanner::scan(&input_dir, &output_dir) {
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

                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job::run(j, &ctx)))
                        .unwrap_or_else(|panic| Err(anyhow::anyhow!("avet panicked: {}", panic_message(&*panic))));
                    if let Err(e) = result {
                        job::handle_failure(j, &ctx, stem, &e, shutdown.load(Ordering::Relaxed));
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

    let mut signals = match Signals::new([SIGTERM, SIGINT]) {
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
            tracing::info!("signal received - finishing the current job, then stopping");
        }
    });

    flag
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
