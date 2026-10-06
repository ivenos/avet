use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::config::{SubtitleConfig, SubtitleMode};

const MATROSKA_SUBTITLES: &[&str] = &[
    "subrip", "text", "ass", "webvtt", "dvd_subtitle", "dvb_subtitle", "hdmv_pgs_subtitle",
    "hdmv_text_subtitle", "arib_caption",
];

#[derive(Default)]
pub struct SubtitlePlan {
    pub extract: Vec<(usize, Option<&'static str>)>,
    /// Stream index and mkvmerge track ID.
    pub from_source: Vec<(usize, u64)>,
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
        let tracks = identify(source, "subtitles")?;
        for (index, id, codec) in unsupported {
            let matched = match_track(&tracks, index, id.as_deref());
            for track in &matched {
                if selected(track.language.as_deref()) {
                    plan.from_source.push((index, track.id));
                }
            }
            if matched.is_empty() {
                tracing::warn!("subtitle stream {} ({codec}) cannot be stored in Matroska - skipped",
                    id.as_deref().unwrap_or("?"));
            }
        }
    }

    if !config.language_whitelist.is_empty() && total > 0 && plan.extract.is_empty() && plan.from_source.is_empty() {
        tracing::warn!(
            "subtitles: the language whitelist {:?} matched none of the {total} subtitle track(s) - the output has none",
            config.language_whitelist
        );
    }
    Ok(plan)
}

/// By mkvmerge's track number where ffprobe reports one. The matroska demuxer does not,
/// so there the Nth subtitle stream is the Nth subtitle track.
pub fn match_track<'a>(tracks: &'a [Identified], index: usize, id: Option<&str>) -> Vec<&'a Identified> {
    let number = id.and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok());
    if let Some(number) = number {
        return tracks.iter().filter(|t| t.number == Some(number)).collect();
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

pub struct Identified {
    pub id: u64,
    pub number: Option<u64>,
    pub language: Option<String>,
}

pub fn identify(source: &Path, track_type: &str) -> Result<Vec<Identified>> {
    let mut cmd = crate::ext::mkvmerge();
    cmd.args(["--identify", "--identification-format", "json"]).arg(source);
    let out = crate::ext::mkvmerge_output(&mut cmd, 300, "mkvmerge identify")?;

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
        .filter(|t| t.track_type == track_type)
        .map(|t| Identified { id: t.id, number: t.properties.number, language: t.properties.language })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matroska_track_is_matched_by_position_because_ffprobe_reports_no_id() {
        let track = |id, number| Identified { id, number: Some(number), language: None };
        let tracks = vec![track(2, 3), track(3, 4)];

        // MP4 and MPEG-TS: ffprobe prints the id, and the number decides.
        assert_eq!(match_track(&tracks, 0, Some("0x4"))[0].id, 3);
        assert_eq!(match_track(&tracks, 1, Some("0x3"))[0].id, 2);

        // Matroska: no id at all, so the second subtitle stream is the second track.
        assert_eq!(match_track(&tracks, 1, None)[0].id, 3);
        assert_eq!(match_track(&tracks, 0, None)[0].id, 2);
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
