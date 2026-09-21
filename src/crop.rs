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
        let handles: Vec<_> = [5u64, 20, 40, 60, 80, 95]
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
    if samples.is_empty() {
        if failed > 0 {
            bail!("auto-crop: all {failed} cropdetect samples failed");
        }
        // Nothing was measured, so there is nothing worth caching either.
        tracing::warn!("[{stem}] auto-crop: no sample produced a crop box - leaving the video as it is");
        return Ok(None);
    }

    // Union, not majority: a narrower agreement cuts content only one sample saw.
    let union = samples
        .into_iter()
        .reduce(Crop::union)
        .and_then(|c| c.normalized(orig_w, orig_h));

    let (detected, mut cacheable) = classify(union, orig_w, orig_h);
    if !cacheable {
        tracing::warn!(
            "[{stem}] auto-crop: ignoring implausible {} for a {orig_w}x{orig_h} source",
            union.map(|c| c.to_filter()).unwrap_or_default()
        );
    }
    // A cached box outlives the retry that would measure the missing samples.
    if failed > 0 {
        tracing::warn!("[{stem}] auto-crop: {failed} sample(s) failed - measuring again next time");
        cacheable = false;
    }

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

/// `(crop, cacheable)`. cropdetect boxes what is not black, so an all-dark scene boxes
/// the only lit part; caching that would keep it for the whole film.
fn classify(union: Option<Crop>, src_w: u32, src_h: u32) -> (Option<Crop>, bool) {
    let src_area = u64::from(src_w) * u64::from(src_h);
    let area = |c: &Crop| u64::from(c.w) * u64::from(c.h);
    match union {
        Some(c) if area(&c) * 100 < src_area * 40 => (None, false),
        Some(c) if !is_bars(&c, src_w, src_h) => (None, false),
        Some(c) if area(&c) * 100 >= src_area * 99 => (None, true),
        other => (other, true),
    }
}

/// Bars sit roughly opposite each other. A box offset to one side is the lit part of a
/// dark scene, and cutting to it would take real picture off the other side. The margin
/// is wide: a transfer's bars are often a few lines apart, deliberately so in the fixtures.
fn is_bars(c: &Crop, src_w: u32, src_h: u32) -> bool {
    let centered = |near: u32, far: u32, total: u32| {
        let slack = (total / 20).max(8);
        near.abs_diff(far) <= slack
    };
    centered(c.x, src_w.saturating_sub(c.x + c.w), src_w)
        && centered(c.y, src_h.saturating_sub(c.y + c.h), src_h)
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
    // FFMS2 decodes in storage orientation; autorotate would box the displayed image.
    cmd.args(["-noautorotate", "-ss", &seek_secs.to_string()])
        .arg("-i").arg(source_file)
        // Same track as probe_dimensions; ffmpeg's own pick is by resolution.
        .args(["-map", "0:v:0"])
        // Below 1.0 ffmpeg scales the limit by the bit depth; round=16 would report
        // 640x352 for a clean 640x360 source.
        .args(["-t", "10", "-vf", "cropdetect=0.094:2:0", "-f", "null", "-"]);
    let output = crate::ext::output_with_timeout(&mut cmd, TIMEOUT_SECS, "ffmpeg cropdetect")?;

    if !output.status.success() {
        return Err(crate::ext::tool_error(
            &format!("ffmpeg cropdetect at {seek_secs}s"),
            output.status,
            &String::from_utf8_lossy(&output.stderr),
        ));
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
    if let Err(e) = crate::resume::write_atomic(path, content.as_bytes()) {
        tracing::warn!("could not write crop cache {}: {e:#}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_letterbox_is_a_crop_and_a_lit_corner_of_a_dark_scene_is_not() {
        let crop = |w, h, x, y| Some(Crop { w, h, x, y });

        assert_eq!(classify(crop(1920, 800, 0, 140), 1920, 1080), (crop(1920, 800, 0, 140), true));
        assert_eq!(classify(crop(1440, 1080, 240, 0), 1920, 1080), (crop(1440, 1080, 240, 0), true));

        assert_eq!(classify(crop(640, 360, 100, 100), 1920, 1080), (None, false));
        assert_eq!(classify(crop(1920, 1072, 0, 4), 1920, 1080), (None, true));
        assert_eq!(classify(None, 1920, 1080), (None, true));
    }

    #[test]
    fn a_box_that_is_not_centered_is_a_lit_scene_and_not_a_bar() {
        let crop = |w, h, x, y| Some(Crop { w, h, x, y });

        // Both are large enough to clear the area check on their own.
        assert_eq!(classify(crop(1400, 700, 100, 50), 1920, 1080), (None, false));
        assert_eq!(classify(crop(1500, 1080, 420, 0), 1920, 1080), (None, false));

        // Bars a few lines apart are still bars, which pattern_bars.mkv relies on.
        assert_eq!(classify(crop(640, 276, 0, 44), 640, 360), (crop(640, 276, 0, 44), true));
        assert_eq!(classify(crop(1920, 800, 0, 140), 1920, 1080), (crop(1920, 800, 0, 140), true));
        // Windowboxed: both axes cut, both roughly centered.
        assert_eq!(classify(crop(1440, 800, 240, 140), 1920, 1080), (crop(1440, 800, 240, 140), true));
    }
}
