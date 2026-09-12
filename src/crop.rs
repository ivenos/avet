use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::ext::external_bin;
use crate::ffms2::Crop;

/// "crop=W:H:X:Y", or None if there is nothing to cut. Cached in the job's temp dir.
pub fn detect(
    source_file: &Path,
    duration_secs: f64,
    cache_path: &Path,
    stem: &str,
) -> Result<Option<String>> {
    if cache_path.exists() {
        let cached = std::fs::read_to_string(cache_path)
            .unwrap_or_default()
            .trim()
            .to_string();
        if cached.is_empty() {
            tracing::info!("[{stem}] auto-crop: no black bars (cached)");
        } else {
            tracing::info!("[{stem}] auto-crop: {cached} (cached)");
        }
        return Ok(if cached.is_empty() { None } else { Some(cached) });
    }

    let (orig_w, orig_h) = probe_dimensions(source_file)?;

    tracing::info!("[{stem}] auto-crop: running cropdetect...");

    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = [10u64, 25, 40, 55, 70]
            .iter()
            .map(|&pct| {
                let seek = (duration_secs * pct as f64 / 100.0) as u64;
                s.spawn(move || run_cropdetect(source_file, seek))
            })
            .collect();
        handles.into_iter().map(|h| h.join()).collect()
    });

    let mut samples: Vec<Crop> = Vec::new();
    let mut failed = 0usize;
    for result in results {
        match result {
            Ok(Ok(Some(c))) => samples.push(c),
            Ok(Ok(None))    => {}
            Ok(Err(e))      => { failed += 1; tracing::warn!("[{stem}] cropdetect sample failed: {e:#}"); }
            Err(_)          => { failed += 1; tracing::warn!("[{stem}] cropdetect sample panicked"); }
        }
    }

    // A failure is not evidence of "no black bars", and the cache survives resumes.
    if samples.is_empty() && failed > 0 {
        bail!("auto-crop: all {failed} cropdetect samples failed");
    }

    // Union, not majority: a narrower agreement cuts content only one sample saw.
    let union = samples
        .into_iter()
        .reduce(Crop::union)
        .and_then(|c| c.normalized(orig_w, orig_h));

    let src_area = u64::from(orig_w) * u64::from(orig_h);
    let area = |c: &Crop| u64::from(c.w) * u64::from(c.h);

    let (detected, cacheable) = match union {
        // cropdetect boxes what is not black, so an all-dark scene boxes the only lit part.
        Some(c) if area(&c) * 100 < src_area * 40 => {
            tracing::warn!(
                "[{stem}] auto-crop: ignoring implausible {} for a {orig_w}x{orig_h} source",
                c.to_filter()
            );
            (None, false)
        }
        // Rounding noise, not a crop.
        Some(c) if area(&c) * 100 >= src_area * 99 => (None, true),
        other => (other, true),
    };

    let result = detected.map(|c| c.to_filter());
    if cacheable {
        cache_result(cache_path, result.as_deref().unwrap_or(""));
    }

    match &result {
        Some(c) => tracing::info!("[{stem}] auto-crop: detected {c}"),
        None    => tracing::info!("[{stem}] auto-crop: no black bars detected"),
    }

    Ok(result)
}

fn probe_dimensions(source_file: &Path) -> Result<(u32, u32)> {
    #[derive(serde::Deserialize)]
    struct Root { streams: Vec<Stream> }
    #[derive(serde::Deserialize)]
    struct Stream { width: u32, height: u32 }

    let root: Root = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "v:0",
          "-show_entries", "stream=width,height", "-of", "json"],
        source_file,
    )
    .context("auto-crop: probe source dimensions")?;

    root.streams
        .into_iter()
        .next()
        .map(|s| (s.width, s.height))
        .filter(|&(w, h)| w > 0 && h > 0)
        .context("auto-crop: source has no video stream with a size")
}

/// The last cropdetect box of one sample, which is its cumulative bounding box.
fn run_cropdetect(source_file: &Path, seek_secs: u64) -> Result<Option<Crop>> {
    const TIMEOUT_SECS: u64 = 300;

    let mut cmd = std::process::Command::new(external_bin("ffmpeg"));
    cmd.args(["-ss", &seek_secs.to_string()])
        .arg("-i").arg(source_file)
        // Same track as probe_dimensions; ffmpeg's own pick is by resolution.
        .args(["-map", "0:v:0"])
        // Below 1.0 ffmpeg scales the limit by the bit depth; round=16 would report
        // 640x352 for a clean 640x360 source.
        .args(["-t", "10", "-vf", "cropdetect=0.094:2:0", "-f", "null", "-"]);
    let output = crate::ext::output_with_timeout(&mut cmd, TIMEOUT_SECS, "ffmpeg cropdetect")?;

    if !output.status.success() {
        bail!(
            "ffmpeg cropdetect failed at {seek_secs}s:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // cropdetect writes results to stderr
    Ok(String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| {
            let pos = line.find("crop=")?;
            Crop::from_str(line[pos..].split_whitespace().next()?)
        })
        .next_back())
}

fn cache_result(path: &Path, content: &str) {
    if let Err(e) = std::fs::write(path, content) {
        tracing::warn!("could not write crop cache {}: {e:#}", path.display());
    }
}
