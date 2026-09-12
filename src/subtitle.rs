use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

use crate::config::{SubtitleConfig, SubtitleMode};
use crate::ext::external_bin;

pub enum SubtitleSelection {
    Strip,
    All,
    Indices(Vec<usize>),
}

#[derive(Deserialize)]
struct SubProbe { streams: Vec<SubStream> }
#[derive(Deserialize)]
struct SubStream { #[serde(default)] tags: SubTags }
#[derive(Deserialize, Default)]
struct SubTags { language: Option<String> }

pub fn select_tracks(source: &Path, config: &SubtitleConfig) -> Result<SubtitleSelection> {
    if config.mode == SubtitleMode::Strip {
        return Ok(SubtitleSelection::Strip);
    }
    if config.language_whitelist.is_empty() {
        return Ok(SubtitleSelection::All);
    }

    let probe = probe_subtitle_langs(source)?;
    let indices: Vec<usize> = probe.streams.iter().enumerate()
        .filter(|(_, s)| {
            crate::config::language_selected(
                &config.language_whitelist,
                s.tags.language.as_deref(),
            )
        })
        .map(|(i, _)| i)
        .collect();

    Ok(SubtitleSelection::Indices(indices))
}

/// ffprobe subtitle languages, retried since the probe can fail transiently.
fn probe_subtitle_langs(source: &Path) -> Result<SubProbe> {
    let args = &["-v", "error", "-select_streams", "s",
                 "-show_entries", "stream_tags=language", "-of", "json"];
    let mut last = String::new();
    for attempt in 1..=3 {
        match crate::ext::ffprobe_json::<SubProbe>(args, source) {
            Ok(p) => return Ok(p),
            Err(e) => {
                last = e.to_string();
                tracing::warn!("ffprobe subtitle probe attempt {attempt}/3 failed: {last}");
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
    bail!("ffprobe subtitle probe failed after 3 attempts: {last}");
}

pub(crate) fn probe_track_ids(source: &Path, subtitle_indices: &[usize]) -> Result<Vec<u64>> {
    let mut cmd = Command::new(external_bin("mkvmerge"));
    cmd.args(["--identify", "--identification-format", "json"]).arg(source);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "mkvmerge --identify")?;

    if out.status.code().unwrap_or(2) >= 2 {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("mkvmerge identify failed:\n{stderr}");
    }

    #[derive(Deserialize)]
    struct Identify { tracks: Vec<Track> }
    #[derive(Deserialize)]
    struct Track { id: u64, #[serde(rename = "type")] track_type: String }

    let identified: Identify = serde_json::from_slice(&out.stdout)
        .context("parse mkvmerge identify output")?;

    let subtitle_ids: Vec<u64> = identified.tracks.iter()
        .filter(|t| t.track_type == "subtitles")
        .map(|t| t.id)
        .collect();

    Ok(subtitle_indices.iter()
        .filter_map(|&i| match subtitle_ids.get(i) {
            Some(&id) => Some(id),
            None => {
                tracing::warn!("subtitle index {i} out of range ({} tracks) - skipped", subtitle_ids.len());
                None
            }
        })
        .collect())
}
