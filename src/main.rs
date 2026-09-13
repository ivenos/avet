mod audio;
mod config;
mod crop;
mod encode;
mod ext;
mod ffms2;
mod hdr;
mod job;
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
                    let stem = j.stem();

                    if let Err(e) = job::run(j, &ctx) {
                        job::handle_failure(j, &ctx, stem, &e);
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
        if signals.forever().next().is_some() {
            tracing::info!("signal received - finishing the current job, then stopping");
            set.store(true, Ordering::Relaxed);
        }
    });

    flag
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var(var)
        .unwrap_or_else(|_| default.to_string())
        .into()
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
