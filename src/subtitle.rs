use anyhow::{Context, Result};
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
    let mut total = 0usize;
    for (index, stream) in probe.streams.into_iter().enumerate() {
        total += 1;
        let codec = stream.codec_name.unwrap_or_else(|| "unknown".into());
        match route(&codec) {
            Route::ExtractWithFfmpeg(convert) if selected(stream.tags.language.as_deref()) => {
                plan.extract.push((index, convert));
            }
            Route::ExtractWithFfmpeg(_) => {}
            Route::FromSourceWithMkvmerge => unsupported.push((index, stream.id, codec)),
        }
    }
    if !unsupported.is_empty() {
        let tracks = identify_subtitles(source)?;
        for (index, id, codec) in unsupported {
            let matched = match_track(&tracks, index, id.as_deref());
            for track in &matched {
                if selected(track.2.as_deref()) {
                    plan.from_source.push(track.0);
                }
            }
            if matched.is_empty() {
                tracing::warn!("subtitle stream {} ({codec}) cannot be stored in Matroska - skipped",
                    id.as_deref().unwrap_or("?"));
            }
        }
    }

    // Filtering every track away is a plausible profile, but rarely the intent: a
    // whitelist in ISO 639-1 ("en") never matches a three-letter tag.
    if total > 0 && plan.extract.is_empty() && plan.from_source.is_empty() {
        tracing::warn!(
            "subtitles: the language whitelist {:?} matched none of the {total} subtitle track(s) - the output has none",
            config.language_whitelist
        );
    }
    Ok(plan)
}

/// By mkvmerge's track number where ffprobe reports one. The matroska demuxer does not,
/// so there the Nth subtitle stream is the Nth subtitle track.
fn match_track<'a>(
    tracks: &'a [(u64, Option<u64>, Option<String>)],
    index: usize,
    id: Option<&str>,
) -> Vec<&'a (u64, Option<u64>, Option<String>)> {
    let number = id.and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok());
    if let Some(number) = number {
        return tracks.iter().filter(|(_, n, _)| *n == Some(number)).collect();
    }
    tracks.get(index).into_iter().collect()
}

#[derive(Debug, PartialEq)]
enum Route {
    ExtractWithFfmpeg(Option<&'static str>),
    FromSourceWithMkvmerge,
}

fn route(codec: &str) -> Route {
    match codec {
        c if MATROSKA_SUBTITLES.contains(&c) => Route::ExtractWithFfmpeg(None),
        "mov_text" => Route::ExtractWithFfmpeg(Some("srt")),
        _ => Route::FromSourceWithMkvmerge,
    }
}

fn identify_subtitles(source: &Path) -> Result<Vec<(u64, Option<u64>, Option<String>)>> {
    let mut cmd = Command::new(external_bin("mkvmerge"));
    cmd.args(["--identify", "--identification-format", "json"]).arg(source);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "mkvmerge --identify")?;

    if out.status.code().unwrap_or(2) >= 2 {
        return Err(crate::ext::tool_error("mkvmerge identify", out.status, &String::from_utf8_lossy(&out.stdout)));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matroska_track_is_matched_by_position_because_ffprobe_reports_no_id() {
        let tracks = vec![
            (2u64, Some(3u64), Some("eng".to_string())),
            (3u64, Some(4u64), Some("jpn".to_string())),
        ];

        // MP4 and MPEG-TS: ffprobe prints the id, and the number decides.
        assert_eq!(match_track(&tracks, 0, Some("0x4"))[0].0, 3);
        assert_eq!(match_track(&tracks, 1, Some("0x3"))[0].0, 2);

        // Matroska: no id at all, so the second subtitle stream is the second track.
        assert_eq!(match_track(&tracks, 1, None)[0].0, 3);
        assert_eq!(match_track(&tracks, 0, None)[0].0, 2);
        assert!(match_track(&tracks, 2, None).is_empty());

        // An id mkvmerge does not list stays unmatched rather than falling back.
        assert!(match_track(&tracks, 0, Some("0x99")).is_empty());
    }

    #[test]
    fn only_what_ffmpeg_can_write_into_matroska_is_extracted() {
        for codec in ["subrip", "ass", "webvtt", "dvd_subtitle", "hdmv_pgs_subtitle", "dvb_subtitle"] {
            assert_eq!(route(codec), Route::ExtractWithFfmpeg(None), "{codec} should be extracted as it is");
        }
        assert_eq!(route("mov_text"), Route::ExtractWithFfmpeg(Some("srt")));

        for codec in ["ttml", "dvb_teletext", "eia_608", "unknown"] {
            assert_eq!(route(codec), Route::FromSourceWithMkvmerge, "{codec} should go through mkvmerge");
        }
    }
}
