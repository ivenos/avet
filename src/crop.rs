use anyhow::{bail, Result};
use std::path::Path;

use crate::ext::external_bin;
use crate::ffms2::Crop;

/// "crop=W:H:X:Y" in FFMS2's frame, or None if `visible` has nothing to cut. Cached in the job's temp dir.
pub fn detect(
    source_file: &Path,
    duration_secs: f64,
    cache_path: &Path,
    stem: &str,
    visible: Crop,
) -> Result<Option<String>> {
    // Read as "no black bars", an unreadable cache would change the fingerprint and discard the chunks.
    match std::fs::read_to_string(cache_path) {
        Ok(cached) => {
            let cached = cached.trim().to_string();
            if cached.is_empty() {
                tracing::info!("[{stem}] auto-crop: no black bars (cached)");
            } else {
                tracing::info!("[{stem}] auto-crop: {cached} (cached)");
            }
            return Ok(if cached.is_empty() { None } else { Some(cached) });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("[{stem}] auto-crop: cannot read {} ({e}) - measuring again", cache_path.display()),
    }

    let (orig_w, orig_h) = (visible.w, visible.h);

    tracing::info!("[{stem}] auto-crop: running cropdetect...");

    // The keyframes of the whole file catch what the windows miss, such as a film's IMAX scenes.
    let windows = [5u64, 20, 40, 60, 80, 95].map(|pct| Some((duration_secs * pct as f64 / 100.0) as u64));
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = windows
            .into_iter()
            .chain([None])
            .map(|seek| s.spawn(move || run_cropdetect(source_file, seek)))
            .collect();
        handles.into_iter().map(|h| h.join()).collect()
    });

    let mut samples: Vec<Crop> = Vec::new();
    let mut failed = 0usize;
    let mut transient = false;
    for result in results {
        match result {
            Ok(Ok(Some(c))) => samples.push(c),
            Ok(Ok(None))    => {}
            Ok(Err(e))      => {
                failed += 1;
                transient |= e.downcast_ref::<crate::job::Transient>().is_some();
                tracing::warn!("[{stem}] cropdetect sample failed: {e:#}");
            }
            Err(_)          => { failed += 1; tracing::warn!("[{stem}] cropdetect sample panicked"); }
        }
    }

    // A failure is not evidence of "no black bars", and the crop is in the fingerprint.
    if transient {
        return Err(anyhow::Error::new(crate::job::Transient)
            .context(format!("auto-crop: {failed} cropdetect sample(s) failed")));
    }
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

    let result = detected.map(|c| Crop { x: c.x + visible.x, y: c.y + visible.y, ..c }.to_filter());
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

/// Bars sit roughly opposite each other; a box offset to one side is the lit part of a dark
/// scene. The margin is wide because a transfer's bars are often a few lines apart.
fn is_bars(c: &Crop, src_w: u32, src_h: u32) -> bool {
    let centered = |near: u32, far: u32, total: u32| {
        let slack = (total / 20).max(8);
        near.abs_diff(far) <= slack
    };
    centered(c.x, src_w.saturating_sub(c.x + c.w), src_w)
        && centered(c.y, src_h.saturating_sub(c.y + c.h), src_h)
}

/// The last box, which cropdetect accumulates: ten seconds from `seek_secs`, or every keyframe.
fn run_cropdetect(source_file: &Path, seek_secs: Option<u64>) -> Result<Option<Crop>> {
    let mut cmd = std::process::Command::new(external_bin("ffmpeg"));
    // FFMS2 decodes in storage orientation; autorotate would box the displayed image.
    cmd.arg("-noautorotate");
    let timeout = match seek_secs {
        Some(seek) => {
            cmd.args(["-ss", &seek.to_string(), "-t", "10"]);
            300
        }
        None => {
            cmd.args(["-skip_frame", "nokey"]);
            crate::ext::whole_file_timeout(source_file, 1800)
        }
    };
    cmd.arg("-i").arg(source_file)
        // The track FFMS2 indexes; ffmpeg's own pick is by resolution.
        .args(["-map", "0:v:0"])
        // Below 1.0 ffmpeg scales the limit by the bit depth; round=16 would report
        // 640x352 for a clean 640x360 source.
        .args(["-vf", "cropdetect=0.094:2:0", "-f", "null", "-"]);
    let what = seek_secs.map_or_else(|| "ffmpeg cropdetect on the keyframes".to_string(), |s| format!("ffmpeg cropdetect at {s}s"));
    let output = crate::ext::output_with_timeout(&mut cmd, timeout, &what)?;

    if !output.status.success() {
        return Err(crate::ext::tool_error(&what, output.status, &String::from_utf8_lossy(&output.stderr)));
    }

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
    fn a_cache_that_cannot_be_read_is_not_read_as_no_black_bars() {
        let dir = tempfile::TempDir::new().unwrap();
        let whole = Crop { w: 640, h: 360, x: 0, y: 0 };
        let cached = |path: &Path| detect(Path::new("/nonexistent/film.mkv"), 60.0, path, "film", whole);

        let cache = dir.path().join("crop.cache");
        std::fs::write(&cache, "crop=640:276:0:42\n").unwrap();
        assert_eq!(cached(&cache).unwrap().as_deref(), Some("crop=640:276:0:42"));
        std::fs::write(&cache, "").unwrap();
        assert_eq!(cached(&cache).unwrap(), None);

        let unreadable = dir.path().join("as-a-dir");
        std::fs::create_dir(&unreadable).unwrap();
        assert!(cached(&unreadable).is_err(), "an unreadable cache passed as 'no black bars'");
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
