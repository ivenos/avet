use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

use crate::config::{SubtitleConfig, SubtitleMode};
use crate::ext::external_bin;

const MATROSKA_SUBTITLES: &[&str] = &[
    "subrip", "text", "ass", "webvtt", "dvd_subtitle", "dvb_subtitle", "hdmv_pgs_subtitle",
    "hdmv_text_subtitle", "arib_caption",
];

#[derive(Default)]
pub struct SubtitlePlan {
    pub extract: Vec<(usize, Option<&'static str>)>,
    pub from_source: Vec<u64>,
}

pub fn plan(source: &Path, config: &SubtitleConfig) -> Result<SubtitlePlan> {
    if config.mode == SubtitleMode::Strip {
        return Ok(SubtitlePlan::default());
    }

    #[derive(Deserialize)]
    struct Probe { streams: Vec<Stream> }
    #[derive(Deserialize)]
    struct Stream {
        id: Option<String>,
        codec_name: Option<String>,
        #[serde(default)]
        tags: Tags,
    }
    #[derive(Deserialize, Default)]
    struct Tags { language: Option<String> }

    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "s",
          "-show_entries", "stream=id,codec_name:stream_tags=language", "-of", "json"],
        source,
    )
    .context("probe subtitle streams")?;

    let selected = |language: Option<&str>| {
        crate::config::language_selected(&config.language_whitelist, language)
    };
    let mut plan = SubtitlePlan::default();
    let mut unsupported = Vec::new();
    for (index, stream) in probe.streams.into_iter().enumerate() {
        let codec = stream.codec_name.unwrap_or_else(|| "unknown".into());
        if MATROSKA_SUBTITLES.contains(&codec.as_str()) {
            if selected(stream.tags.language.as_deref()) {
                plan.extract.push((index, None));
            }
        } else if codec == "mov_text" {
            if selected(stream.tags.language.as_deref()) {
                plan.extract.push((index, Some("srt")));
            }
        } else {
            unsupported.push((stream.id, codec));
        }
    }
    if unsupported.is_empty() {
        return Ok(plan);
    }

    // mkvmerge's track number (PID, MP4 track ID, Matroska track number) is ffprobe's stream ID.
    let tracks = identify_subtitles(source)?;
    for (id, codec) in unsupported {
        let number = id.as_deref()
            .and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok());
        let matching: Vec<&(u64, Option<u64>, Option<String>)> =
            tracks.iter().filter(|(_, n, _)| number.is_some() && *n == number).collect();
        if matching.is_empty() {
            tracing::warn!("subtitle stream {} ({codec}) cannot be stored in Matroska - skipped",
                id.as_deref().unwrap_or("?"));
        }
        plan.from_source.extend(
            matching.into_iter().filter(|(_, _, language)| selected(language.as_deref())).map(|(tid, _, _)| *tid),
        );
    }
    Ok(plan)
}

fn identify_subtitles(source: &Path) -> Result<Vec<(u64, Option<u64>, Option<String>)>> {
    let mut cmd = Command::new(external_bin("mkvmerge"));
    cmd.args(["--identify", "--identification-format", "json"]).arg(source);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "mkvmerge --identify")?;

    if out.status.code().unwrap_or(2) >= 2 {
        bail!("mkvmerge identify failed:\n{}", String::from_utf8_lossy(&out.stdout));
    }

    #[derive(Deserialize)]
    struct Identify { tracks: Vec<Track> }
    #[derive(Deserialize)]
    struct Track {
        id: u64,
        #[serde(rename = "type")]
        track_type: String,
        #[serde(default)]
        properties: Properties,
    }
    #[derive(Deserialize, Default)]
    struct Properties { number: Option<u64>, language: Option<String> }

    let identified: Identify = serde_json::from_slice(&out.stdout)
        .context("parse mkvmerge identify output")?;

    Ok(identified.tracks.into_iter()
        .filter(|t| t.track_type == "subtitles")
        .map(|t| (t.id, t.properties.number, t.properties.language))
        .collect())
}
