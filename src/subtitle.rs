use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::audio::Languages;
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

pub fn plan(source: &Path, config: &SubtitleConfig, options: &[&str], stem: &str) -> Result<SubtitlePlan> {
    if config.mode == SubtitleMode::Strip {
        return Ok(SubtitlePlan::default());
    }

    #[derive(Deserialize)]
    struct Probe { streams: Vec<Stream> }
    #[derive(Deserialize)]
    struct Stream {
        index: usize,
        id: Option<String>,
        codec_name: Option<String>,
        #[serde(default)]
        tags: Tags,
    }
    #[derive(Deserialize, Default)]
    struct Tags { language: Option<String> }

    let probe: Probe = crate::ext::ffprobe_json(
        &[options, crate::audio::STREAMS_ONLY, &["-v", "error", "-select_streams", "s",
          "-show_entries", "stream=index,id,codec_name:stream_tags=language", "-of", "json"]].concat(),
        source,
    )
    .context("probe subtitle streams")?;
    let streams = probe.streams.len();
    let other_programs = crate::audio::other_program_streams(source, options, stem)?;

    let selected = |language: Option<&str>| {
        crate::config::language_selected(&config.language_whitelist, language)
    };
    let left_out = |index: usize, language: Option<&str>| {
        tracing::info!("[{stem}] subtitle track {index} ({}) left out by the language whitelist", language.unwrap_or("und"));
    };
    let mut plan = SubtitlePlan::default();
    let mut unsupported = Vec::new();
    let mut languages = Vec::new();
    let mut read = None;
    for (index, stream) in probe.streams.into_iter().enumerate() {
        if other_programs.contains(&stream.index) {
            continue;
        }
        languages.push(stream.tags.language.clone().unwrap_or_else(|| "und".into()));
        let codec = stream.codec_name.unwrap_or_else(|| "unknown".into());
        match route(&codec) {
            Route::ExtractWithFfmpeg(convert) => {
                let legacy = || Ok(Languages::cached(&mut read, source, options)?.legacy_subtitle(index));
                if crate::audio::language_selected(&config.language_whitelist, stream.tags.language.as_deref(), legacy)
                    .with_context(|| format!("match subtitle stream {index} against the language whitelist"))?
                {
                    plan.extract.push((index, convert));
                } else {
                    left_out(index, stream.tags.language.as_deref());
                }
            }
            Route::FromSourceWithMkvmerge => unsupported.push((index, stream.id, codec)),
        }
    }
    if !unsupported.is_empty() {
        let tracks = identify(source, "subtitles", options)?;
        for (index, id, codec) in unsupported {
            let matched = same_track(&tracks, |t| t.number, index, id.as_deref(), streams);
            for track in &matched {
                if selected(track.language.as_deref()) {
                    plan.from_source.push((index, track.id));
                } else {
                    left_out(index, track.language.as_deref());
                }
            }
            if matched.is_empty() {
                tracing::warn!("[{stem}] subtitle stream {} ({codec}) cannot be stored in Matroska - skipped",
                    id.as_deref().unwrap_or("?"));
            }
        }
    }

    if !config.language_whitelist.is_empty() && !languages.is_empty() && plan.extract.is_empty() && plan.from_source.is_empty()
        && !languages.iter().any(|l| selected(Some(l)))
    {
        tracing::warn!(
            "[{stem}] the language whitelist {:?} matched none of the subtitle tracks ({}) - the output has none",
            config.language_whitelist, languages.join(", ")
        );
    }
    Ok(plan)
}

/// By mkvmerge's track number where ffprobe reports a stream id. The matroska demuxer does not,
/// so there the Nth of `streams` streams is the Nth track, if mkvmerge lists as many.
pub fn same_track<'a, T>(tracks: &'a [T], number: impl Fn(&T) -> Option<u64>, index: usize, id: Option<&str>, streams: usize) -> Vec<&'a T> {
    match id.and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok()) {
        Some(n) => tracks.iter().filter(|t| number(t) == Some(n)).collect(),
        None => tracks.get(index).filter(|_| tracks.len() == streams).into_iter().collect(),
    }
}

pub fn is_text(codec: &str) -> bool {
    matches!(codec, "subrip" | "text" | "ass" | "webvtt" | "mov_text")
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

pub fn identify(source: &Path, track_type: &str, options: &[&str]) -> Result<Vec<Identified>> {
    let mut cmd = crate::ext::mkvmerge();
    cmd.args(crate::audio::mkvmerge_probe_range(source, options)).args(["--identify", "--identification-format", "json"]).arg(source);
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
        let matched = |index, id, streams| same_track(&tracks, |t| t.number, index, id, streams).iter().map(|t| t.id).collect::<Vec<_>>();

        // MP4 and MPEG-TS: ffprobe prints the id, and the number decides.
        assert_eq!(matched(0, Some("0x4"), 2), [3]);
        assert_eq!(matched(1, Some("0x3"), 2), [2]);
        assert_eq!(matched(0, Some("0x4"), 3), [3]);

        // Matroska: no id at all, so the second subtitle stream is the second track.
        assert_eq!(matched(1, None, 2), [3]);
        assert_eq!(matched(0, None, 2), [2]);
        assert!(matched(2, None, 2).is_empty());

        // An id mkvmerge does not list stays unmatched rather than falling back.
        assert!(matched(0, Some("0x99"), 2).is_empty());
    }

    #[test]
    fn no_track_is_matched_by_position_when_the_tools_list_a_different_number() {
        let tracks = [Identified { id: 2, number: Some(3), language: None }, Identified { id: 3, number: Some(4), language: None }];
        assert!(same_track(&tracks, |t| t.number, 0, None, 3).is_empty());
        assert!(same_track(&tracks, |t| t.number, 1, None, 1).is_empty());
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
