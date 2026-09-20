use anyhow::{Context, Result};
use av_scenechange::{DetectionOptions, SceneDetectionSpeed, av_decoders};
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::Stdio;

use crate::config::{SceneDetectionConfig, SceneDetectionSpeedConfig};
use crate::ext::external_bin;
use crate::resume::SceneEntry;

pub fn detect(
    source_file: &Path,
    cfg: &SceneDetectionConfig,
    vf_filter: Option<&str>,
    fps: f64,
) -> Result<Vec<SceneEntry>> {
    let actual_vf = build_detection_vf(vf_filter, cfg.downscale_height);

    let mut cmd = std::process::Command::new(external_bin("ffmpeg"));
    // FFMS2 decodes in storage orientation, which is what the crop filter is built for.
    cmd.args(["-hide_banner", "-loglevel", "error", "-noautorotate"])
        .arg("-i")
        .arg(source_file)
        // The track FFMS2 opens; ffmpeg's own pick is by resolution.
        .args(["-map", "0:v:0"]);

    if let Some(ref vf) = actual_vf {
        cmd.args(["-vf", vf]);
    }

    let mut ffmpeg = cmd
        .args(["-pix_fmt", "yuv420p"])
        // yuv4mpegpipe is not a VFR muxer: without this, frames are dropped or doubled.
        .args(["-fps_mode", "passthrough"])
        .args(["-f", "yuv4mpegpipe", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start ffmpeg for scene detection")?;

    let stdout = ffmpeg.stdout.take().expect("ffmpeg stdout unavailable");
    let stderr_handle = {
        let stderr = ffmpeg.stderr.take().expect("ffmpeg stderr unavailable");
        std::thread::spawn(move || {
            let mut buf = String::new();
            BufReader::new(stderr).read_to_string(&mut buf).ok();
            buf
        })
    };
    let reader: Box<dyn Read> = Box::new(BufReader::new(stdout));

    // ffmpeg's message is the only useful part of an "init y4m decoder" failure.
    let abort = |mut ffmpeg: std::process::Child, handle: std::thread::JoinHandle<String>, e: anyhow::Error| {
        let _ = ffmpeg.kill();
        let _ = ffmpeg.wait();
        let stderr = handle.join().unwrap_or_default();
        match stderr.trim() {
            "" => Err(e),
            s  => Err(e.context(format!("ffmpeg: {s}"))),
        }
    };

    let y4m_dec = match y4m::decode(reader).context("init y4m decoder") {
        Ok(d) => d,
        Err(e) => return abort(ffmpeg, stderr_handle, e),
    };
    let decoder_impl = av_decoders::DecoderImpl::Y4m(y4m_dec);
    let mut decoder = match av_decoders::Decoder::from_decoder_impl(decoder_impl)
        .context("create decoder")
    {
        Ok(d) => d,
        Err(e) => return abort(ffmpeg, stderr_handle, e),
    };

    let speed = match cfg.speed {
        SceneDetectionSpeedConfig::Standard => SceneDetectionSpeed::Standard,
        SceneDetectionSpeedConfig::Fast     => SceneDetectionSpeed::Fast,
    };

    let opts = DetectionOptions {
        analysis_speed: speed,
        detect_flashes: true,
        min_scenecut_distance: Some(cfg.min_scene_len),
        max_scenecut_distance: None,
        lookahead_distance: 5,
    };

    let results = av_scenechange::detect_scene_changes::<u8>(&mut decoder, opts, None, None);

    // Close the read end first, or waiting on an undrained writer hangs for good.
    drop(decoder);

    // On error ffmpeg may still be mid-write, and only a kill gets us out of the wait.
    let status = match &results {
        Ok(_) => ffmpeg.wait().context("wait for ffmpeg")?,
        Err(_) => {
            let _ = ffmpeg.kill();
            ffmpeg.wait().context("wait for ffmpeg")?
        }
    };
    let ffmpeg_stderr = stderr_handle.join().unwrap_or_default();
    let results = results.context("av-scenechange failed")?;

    // A truncated Y4M stream reaches the detector as a clean end of input.
    if !status.success() {
        return Err(crate::ext::tool_error("ffmpeg during scene detection", status, &ffmpeg_stderr));
    }
    if !ffmpeg_stderr.is_empty() {
        tracing::warn!("ffmpeg scene detection: {}", ffmpeg_stderr.trim());
    }

    let scenes = build_scene_entries(&results.scene_changes, results.frame_count.max(1));

    Ok(match cfg.effective_extra_split_frames(fps) {
        Some(max) => apply_extra_split(scenes, max),
        None      => scenes,
    })
}

fn build_detection_vf(base_vf: Option<&str>, downscale_height: Option<u32>) -> Option<String> {
    match (base_vf, downscale_height) {
        (None,    None)    => None,
        (Some(v), None)    => Some(v.to_owned()),
        (None,    Some(h)) => Some(format!("scale=-2:min(ih\\,{h})")),
        (Some(v), Some(h)) => Some(format!("{v},scale=-2:min(ih\\,{h})")),
    }
}

fn apply_extra_split(scenes: Vec<SceneEntry>, max_frames: usize) -> Vec<SceneEntry> {
    let mut result = Vec::new();
    let mut index = 0usize;

    for scene in scenes {
        let len = (scene.end_frame - scene.start_frame + 1) as usize;
        if len <= max_frames {
            result.push(SceneEntry { index, start_frame: scene.start_frame, end_frame: scene.end_frame });
            index += 1;
        } else {
            let n_parts = len.div_ceil(max_frames);
            let part_size = len / n_parts;
            // The remainder goes one frame per part, or the last part would exceed max_frames.
            let long_parts = len % n_parts;
            let mut start = scene.start_frame;
            for i in 0..n_parts {
                let end = start + (part_size + usize::from(i < long_parts)) as u64 - 1;
                result.push(SceneEntry { index, start_frame: start, end_frame: end });
                index += 1;
                start = end + 1;
            }
        }
    }

    result
}

fn build_scene_entries(scene_changes: &[usize], total_frames: usize) -> Vec<SceneEntry> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(scene_changes.iter().copied().filter(|&f| f > 0))
        .collect();

    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts
                .get(i + 1)
                .map(|&s| s - 1)
                .unwrap_or(total_frames - 1);
            SceneEntry {
                index: i,
                start_frame: start as u64,
                end_frame: end as u64,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_entries_single_chunk() {
        let entries = build_scene_entries(&[0], 100);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].start_frame, 0);
        assert_eq!(entries[0].end_frame, 99);
    }

    #[test]
    fn a_single_frame_is_one_chunk() {
        let entries = build_scene_entries(&[], 1);
        assert_eq!((entries.len(), entries[0].start_frame, entries[0].end_frame), (1, 0, 0));
    }

    #[test]
    fn build_entries_two_chunks() {
        let entries = build_scene_entries(&[0, 50], 100);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].start_frame, 0);
        assert_eq!(entries[0].end_frame, 49);
        assert_eq!(entries[1].start_frame, 50);
        assert_eq!(entries[1].end_frame, 99);
    }

    #[test]
    fn build_entries_indices_sequential() {
        let entries = build_scene_entries(&[0, 24, 48, 72], 100);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.index, i);
        }
    }

    #[test]
    fn detection_vf_none_no_downscale() {
        assert_eq!(build_detection_vf(None, None), None);
    }

    #[test]
    fn detection_vf_base_only() {
        assert_eq!(
            build_detection_vf(Some("crop=1920:800:0:140"), None),
            Some("crop=1920:800:0:140".into())
        );
    }

    #[test]
    fn detection_vf_downscale_only() {
        assert_eq!(
            build_detection_vf(None, Some(720)),
            Some("scale=-2:min(ih\\,720)".into())
        );
    }

    #[test]
    fn detection_vf_combined() {
        assert_eq!(
            build_detection_vf(Some("crop=1920:800:0:140"), Some(720)),
            Some("crop=1920:800:0:140,scale=-2:min(ih\\,720)".into())
        );
    }

    #[test]
    fn extra_split_no_split_needed() {
        let scenes = build_scene_entries(&[0, 100], 200);
        let result = apply_extra_split(scenes, 240);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].end_frame, 99);
        assert_eq!(result[1].start_frame, 100);
    }

    #[test]
    fn extra_split_exact_boundary() {
        let scenes = build_scene_entries(&[0], 240);
        let result = apply_extra_split(scenes, 240);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].end_frame, 239);
    }

    #[test]
    fn extra_split_one_over() {
        let scenes = build_scene_entries(&[0], 241);
        let result = apply_extra_split(scenes, 240);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].frame_count(), 121);
        assert_eq!(result[1].frame_count(), 120);
        assert_eq!(result[1].end_frame, 240);
    }

    #[test]
    fn extra_split_triple() {
        let scenes = build_scene_entries(&[0], 720);
        let result = apply_extra_split(scenes, 240);
        assert_eq!(result.len(), 3);
        assert_eq!(result[2].end_frame, 719);
        for e in &result { assert!(e.frame_count() <= 240); }
    }

    #[test]
    fn no_part_exceeds_the_maximum_when_the_scene_does_not_divide_evenly() {
        for (len, max) in [(239usize, 24usize), (1439, 240), (14399, 240), (172799, 240)] {
            let result = apply_extra_split(build_scene_entries(&[0], len), max);
            for e in &result {
                assert!(e.frame_count() <= max as u64, "len {len} max {max}: {}", e.frame_count());
            }
            assert_eq!(result[0].start_frame, 0);
            assert_eq!(result.last().unwrap().end_frame, len as u64 - 1);
            for pair in result.windows(2) {
                assert_eq!(pair[1].start_frame, pair[0].end_frame + 1);
            }
        }
    }

    #[test]
    fn extra_split_reindexes() {
        let scenes = build_scene_entries(&[0, 50], 200);
        let mut long = scenes;
        long[1].end_frame = 800;
        let result = apply_extra_split(long, 100);
        for (i, e) in result.iter().enumerate() {
            assert_eq!(e.index, i);
        }
    }
}
