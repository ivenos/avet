use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use crate::config::{AudioConfig, AudioMode, layout_name, output_is_lossless, toml_value_to_arg};
use crate::ext::external_bin;

const OPUS_LAYOUTS: &[(&str, &[&str])] = &[
    ("mono", &["FC"]),
    ("stereo", &["FL", "FR"]),
    ("3.0", &["FL", "FR", "FC"]),
    ("quad", &["FL", "FR", "BL", "BR"]),
    ("5.0", &["FL", "FR", "FC", "BL", "BR"]),
    ("5.1", &["FL", "FR", "FC", "LFE", "BL", "BR"]),
    ("6.1", &["FL", "FR", "FC", "LFE", "BC", "SL", "SR"]),
    ("7.1", &["FL", "FR", "FC", "LFE", "BL", "BR", "SL", "SR"]),
];

const NAMED_LAYOUTS: &[(&str, &str)] = &[
    ("mono", "FC"), ("stereo", "FL+FR"), ("2.1", "FL+FR+LFE"), ("3.0", "FL+FR+FC"),
    ("3.0(back)", "FL+FR+BC"), ("4.0", "FL+FR+FC+BC"), ("quad", "FL+FR+BL+BR"),
    ("quad(side)", "FL+FR+SL+SR"), ("3.1", "FL+FR+FC+LFE"), ("5.0", "FL+FR+FC+BL+BR"),
    ("5.0(side)", "FL+FR+FC+SL+SR"), ("4.1", "FL+FR+FC+LFE+BC"), ("5.1", "FL+FR+FC+LFE+BL+BR"),
    ("5.1(side)", "FL+FR+FC+LFE+SL+SR"), ("6.0", "FL+FR+FC+BC+SL+SR"),
    ("6.0(front)", "FL+FR+FLC+FRC+SL+SR"), ("3.1.2", "FL+FR+FC+LFE+TFL+TFR"),
    ("hexagonal", "FL+FR+FC+BL+BR+BC"), ("6.1", "FL+FR+FC+LFE+BC+SL+SR"),
    ("6.1(back)", "FL+FR+FC+LFE+BL+BR+BC"), ("6.1(front)", "FL+FR+LFE+FLC+FRC+SL+SR"),
    ("7.0", "FL+FR+FC+BL+BR+SL+SR"), ("7.0(front)", "FL+FR+FC+FLC+FRC+SL+SR"),
    ("7.1", "FL+FR+FC+LFE+BL+BR+SL+SR"), ("7.1(wide)", "FL+FR+FC+LFE+BL+BR+FLC+FRC"),
    ("7.1(wide-side)", "FL+FR+FC+LFE+FLC+FRC+SL+SR"), ("5.1.2", "FL+FR+FC+LFE+SL+SR+TFL+TFR"),
    ("5.1.2(back)", "FL+FR+FC+LFE+BL+BR+TFL+TFR"), ("octagonal", "FL+FR+FC+BL+BR+BC+SL+SR"),
    ("cube", "FL+FR+BL+BR+TFL+TFR+TBL+TBR"), ("downmix", "DL+DR"),
];

#[derive(Deserialize)]
struct FfprobeOutput {
    streams: Vec<FfprobeStream>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    /// ffprobe omits this when it cannot identify the codec.
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    channels: Option<u32>,
    #[serde(default)]
    channel_layout: Option<String>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default)]
    bits_per_raw_sample: Option<String>,
    #[serde(default)]
    sample_fmt: Option<String>,
    #[serde(default)]
    initial_padding: Option<u32>,
    #[serde(default)]
    tags: FfprobeTags,
}

#[derive(Deserialize, Default)]
struct FfprobeTags {
    language: Option<String>,
    title: Option<String>,
}

struct AudioTrack {
    audio_index: usize,
    codec_name: String,
    profile: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    sample_rate: u32,
    bits_per_raw_sample: u32,
    sample_fmt: String,
    initial_padding: u32,
    language: Option<String>,
    title: Option<String>,
}

/// Audio codec names ffmpeg flags as lossless, queried once from `ffmpeg -codecs`.
static LOSSLESS_CODECS: OnceLock<HashSet<String>> = OnceLock::new();

fn lossless_codecs() -> &'static HashSet<String> {
    LOSSLESS_CODECS.get_or_init(|| match probe_lossless_codecs() {
        Ok(set) => set,
        Err(e) => {
            tracing::warn!("ffmpeg -codecs query failed ({e:#}); using built-in lossless list");
            fallback_lossless_codecs()
        }
    })
}

fn probe_lossless_codecs() -> Result<HashSet<String>> {
    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-codecs"]);
    let out = crate::ext::output_with_timeout(&mut cmd, 60, "ffmpeg -codecs")?;
    if !out.status.success() {
        bail!("ffmpeg -codecs exited with failure");
    }
    Ok(parse_lossless_codecs(&String::from_utf8_lossy(&out.stdout)))
}

/// Audio codecs flagged lossless (flag 3 = A, flag 6 = S) in `ffmpeg -codecs`.
fn parse_lossless_codecs(stdout: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let (Some(flags), Some(name)) = (fields.next(), fields.next()) else { continue };
        let f = flags.as_bytes();
        if f.len() >= 6 && f[2] == b'A' && f[5] == b'S' {
            set.insert(name.to_string());
        }
    }
    set
}

fn fallback_lossless_codecs() -> HashSet<String> {
    [
        "truehd", "mlp", "flac", "alac", "ape", "tta", "wavpack", "tak",
        "shorten", "ralf", "wmalossless", "mp4als", "als", "dts",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// PCM is always lossless; DTS only in its Master Audio profile.
fn is_lossless(codec_name: &str, profile: Option<&str>, lossless: &HashSet<String>) -> bool {
    if codec_name == "dts" {
        return profile.is_some_and(|p| p.contains("MA"));
    }
    codec_name.starts_with("pcm_") || lossless.contains(codec_name)
}

fn codec_display(codec: &str) -> &str {
    match codec {
        "libopus" | "opus" => "Opus",
        "flac" => "FLAC",
        "aac" | "libfdk_aac" => "AAC",
        "ac3" => "AC3",
        "eac3" => "E-AC-3",
        "libmp3lame" | "mp3" => "MP3",
        "alac" => "ALAC",
        "libvorbis" | "vorbis" => "Vorbis",
        other => other,
    }
}

#[derive(Deserialize, Default)]
struct FfprobeDisposition {
    #[serde(default)] default: i32,
    #[serde(default)] forced: i32,
    #[serde(default)] hearing_impaired: i32,
    #[serde(default)] visual_impaired: i32,
    #[serde(default)] original: i32,
    #[serde(default)] comment: i32,
}

impl FfprobeDisposition {
    fn to_mkvmerge_flags(&self, tid: usize) -> Vec<String> {
        let t = tid.to_string();
        let yn = |v: i32| if v != 0 { "yes" } else { "no" };
        let mut flags = vec![
            "--default-track-flag".into(), format!("{t}:{}", yn(self.default)),
        ];
        let extras: &[(&str, i32)] = &[
            ("--forced-display-flag",    self.forced),
            ("--hearing-impaired-flag",  self.hearing_impaired),
            ("--visual-impaired-flag",   self.visual_impaired),
            ("--original-flag",          self.original),
            ("--commentary-flag",        self.comment),
        ];
        for (flag, val) in extras {
            if *val != 0 {
                flags.extend([(*flag).to_string(), format!("{t}:yes")]);
            }
        }
        flags
    }
}

#[derive(Deserialize)]
struct FfprobeDispStream {
    #[serde(default)]
    disposition: FfprobeDisposition,
}

#[derive(Deserialize)]
struct FfprobeDispOutput {
    #[serde(default)]
    streams: Vec<FfprobeDispStream>,
}

fn probe_dispositions(path: &Path, stream_spec: &str) -> Vec<FfprobeDisposition> {
    match crate::ext::ffprobe_json::<FfprobeDispOutput>(
        &["-v", "error", "-select_streams", stream_spec,
          "-show_entries", "stream=disposition", "-of", "json"],
        path,
    ) {
        Ok(p) => p.streams.into_iter().map(|s| s.disposition).collect(),
        Err(e) => {
            tracing::warn!("ffprobe disposition probe failed for {}: {e:#}", path.display());
            vec![]
        }
    }
}

fn probe_audio_tracks(source_file: &Path) -> Result<Vec<AudioTrack>> {
    let parsed: FfprobeOutput = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "a",
          "-show_entries",
          "stream=codec_name,profile,channels,channel_layout,sample_rate,bits_per_raw_sample,sample_fmt,initial_padding:stream_tags=language,title",
          "-of", "json"],
        source_file,
    )?;

    let number = |s: Option<String>| s.and_then(|s| s.parse().ok()).unwrap_or(0);
    Ok(parsed
        .streams
        .into_iter()
        .enumerate()
        .map(|(i, s)| AudioTrack {
            audio_index: i,
            codec_name: s.codec_name.unwrap_or_else(|| "unknown".into()),
            profile: s.profile,
            channels: s.channels,
            channel_layout: s.channel_layout,
            sample_rate: number(s.sample_rate),
            bits_per_raw_sample: number(s.bits_per_raw_sample),
            sample_fmt: s.sample_fmt.unwrap_or_default(),
            initial_padding: s.initial_padding.unwrap_or(0),
            language: s.tags.language,
            title: s.tags.title,
        })
        .collect())
}

fn copies_into_matroska(source_file: &Path, audio_index: usize) -> Result<bool> {
    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source_file)
        .args(["-map", &format!("0:a:{audio_index}"), "-c", "copy", "-frames:a", "1", "-f", "matroska", "-"]);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "ffmpeg copy check")?;
    Ok(out.status.success())
}

fn pcm_codec(track: &AudioTrack) -> &'static str {
    match (track.sample_fmt.as_str(), track.bits_per_raw_sample) {
        ("flt" | "fltp", _) => "pcm_f32le",
        ("dbl" | "dblp", _) => "pcm_f64le",
        (_, 1..=16) | ("u8" | "u8p" | "s16" | "s16p", 0) => "pcm_s16le",
        (_, 17..=24) => "pcm_s24le",
        _ => "pcm_s32le",
    }
}

/// Encoder priming or audio before an MP4 edit; a codec delay reaches Matroska by itself.
fn preroll_packets(source_file: &Path, track: &AudioTrack) -> Result<usize> {
    #[derive(Deserialize)]
    struct Probe { #[serde(default)] packets: Vec<Packet> }
    #[derive(Deserialize)]
    struct Packet {
        duration_time: Option<String>,
        #[serde(default)]
        side_data_list: Vec<SideData>,
    }
    #[derive(Deserialize)]
    struct SideData { skip_samples: Option<u64> }

    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", &format!("a:{}", track.audio_index),
          "-read_intervals", "%+30",
          "-show_entries", "packet=duration_time:packet_side_data=skip_samples", "-of", "json"],
        source_file,
    )?;
    let Some(skip) = probe.packets.first().and_then(|p| p.side_data_list.iter().find_map(|s| s.skip_samples)) else {
        return Ok(0);
    };
    if track.initial_padding > 0 {
        return Ok(0);
    }
    let samples = probe.packets.iter().map(|p| {
        p.duration_time.as_deref().and_then(|d| d.parse::<f64>().ok()).map(|d| d * f64::from(track.sample_rate))
    });
    Ok(whole_packets_within(skip as f64, samples))
}

fn whole_packets_within(skip: f64, samples: impl Iterator<Item = Option<f64>>) -> usize {
    let mut covered = 0.0;
    let mut count = 0;
    for n in samples {
        match n {
            Some(n) if n > 0.0 && covered + n / 2.0 < skip => {
                covered += n;
                count += 1;
            }
            _ => break,
        }
    }
    count
}

fn channel_names(layout: &str) -> Option<Vec<&str>> {
    let decomposition = NAMED_LAYOUTS.iter().find(|(name, _)| *name == layout).map_or(layout, |(_, d)| d);
    let names: Vec<&str> = decomposition.split('+').collect();
    names
        .iter()
        .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
        .then_some(names)
}

/// The smallest Opus layout with a place for every source channel, else one by channel count.
fn opus_layout(layout: Option<&str>, channels: Option<u32>) -> (&'static str, String) {
    let substitute = |c: &str| match c {
        "SL" => Some("BL"), "SR" => Some("BR"), "BL" => Some("SL"), "BR" => Some("SR"),
        "DL" => Some("FL"), "DR" => Some("FR"),
        _ => None,
    };
    let source = layout.and_then(channel_names);
    if let Some(source) = &source {
        for (name, target) in OPUS_LAYOUTS {
            let placed: Option<Vec<(&str, &str)>> = source.iter().map(|c| {
                if target.contains(c) {
                    Some((*c, *c))
                } else {
                    substitute(c).filter(|s| target.contains(s) && !source.contains(s)).map(|s| (*c, s))
                }
            }).collect();
            let Some(placed) = placed else { continue };
            let upmix = format!("aformat=channel_layouts={name}");
            if placed.iter().all(|(from, to)| from == to) {
                return (name, upmix);
            }
            let map = placed.iter().map(|(from, to)| format!("{from}-{to}")).collect::<Vec<_>>().join("|");
            return (name, format!("channelmap=map={map},{upmix}"));
        }
    }
    let count = source.map_or(channels.unwrap_or(2), |s| s.len() as u32).clamp(1, 8);
    let name = OPUS_LAYOUTS[count as usize - 1].0;
    (name, format!("aformat=channel_layouts={name}"))
}

/// Drops a trailing "(Marker)" this function added on an earlier run.
fn strip_codec_marker<'a>(title: &'a str, marker: &str) -> &'a str {
    title
        .strip_suffix(')')
        .and_then(|t| t.strip_suffix(marker))
        .and_then(|t| t.strip_suffix('('))
        .map_or(title, str::trim_end)
}

fn track_passes_whitelist(track: &AudioTrack, whitelist: &[String]) -> bool {
    crate::config::language_selected(whitelist, track.language.as_deref())
}

enum Action {
    Copy {
        preroll: usize,
    },
    Pcm {
        codec: &'static str,
    },
    Encode {
        codec: String,
        bitrate: Option<String>,
        options: Vec<(String, String)>,
        layout: Option<(&'static str, String)>,
    },
}

struct PlannedTrack {
    audio_index: usize,
    codec_name: String,
    channels: Option<u32>,
    bits_per_raw_sample: u32,
    language: Option<String>,
    title: Option<String>,
    lossless: bool,
    action: Action,
}

/// Per-track audio decisions, built before the encode and run by `process_plan`.
pub struct AudioPlan {
    tracks: Vec<PlannedTrack>,
}

pub fn plan(source_file: &Path, config: &AudioConfig) -> Result<AudioPlan> {
    let tracks = probe_audio_tracks(source_file)?;
    if tracks.is_empty() {
        return Ok(AudioPlan { tracks: vec![] });
    }

    // An MPEG-TS can announce a stream it never carries.
    let (tracks, empty): (Vec<AudioTrack>, Vec<AudioTrack>) = tracks
        .into_iter()
        .partition(|t| t.sample_rate > 0 && t.channels.unwrap_or(0) > 0);
    for t in empty {
        tracing::warn!(
            "audio track {} ({}) has no sample rate or channel count - skipped",
            t.audio_index, t.codec_name
        );
    }

    let kept: Vec<AudioTrack> = tracks
        .into_iter()
        .filter(|t| track_passes_whitelist(t, &config.language_whitelist))
        .collect();

    if kept.is_empty() {
        tracing::warn!(
            "no audio tracks match language whitelist {:?} - audio omitted",
            config.language_whitelist
        );
        return Ok(AudioPlan { tracks: vec![] });
    }

    let lossless_set = lossless_codecs();
    let mut planned = Vec::with_capacity(kept.len());
    for track in kept {
        let lossless = is_lossless(&track.codec_name, track.profile.as_deref(), lossless_set);
        let r = config.resolve(&track.codec_name, lossless);
        let action = match r.mode {
            AudioMode::Copy if !copies_into_matroska(source_file, track.audio_index)? => {
                Action::Pcm { codec: pcm_codec(&track) }
            }
            AudioMode::Copy => Action::Copy { preroll: preroll_packets(source_file, &track)? },
            AudioMode::Encode => {
                let codec = r.codec.ok_or_else(|| anyhow::anyhow!(
                    "audio track {}: codec is required when mode = encode", track.audio_index
                ))?;
                let bitrate = if output_is_lossless(codec) {
                    None
                } else {
                    let b = r.bitrate.and_then(|b| b.resolve(track.channels)).map(str::to_owned);
                    if b.is_none() {
                        tracing::warn!(
                            "audio track {}: no bitrate for {} channels, using encoder default",
                            track.audio_index,
                            track.channels.map_or_else(|| "?".into(), |c| c.to_string()),
                        );
                    }
                    b
                };
                let mut options: Vec<(String, String)> = r.options
                    .iter()
                    .map(|(k, v)| (k.clone(), toml_value_to_arg(v)))
                    .collect();
                options.sort();
                let layout = codec
                    .contains("opus")
                    .then(|| opus_layout(track.channel_layout.as_deref(), track.channels));
                Action::Encode { codec: codec.to_owned(), bitrate, options, layout }
            }
        };
        planned.push(PlannedTrack {
            audio_index: track.audio_index,
            codec_name: track.codec_name,
            channels: track.channels,
            bits_per_raw_sample: track.bits_per_raw_sample,
            language: track.language,
            title: track.title,
            lossless,
            action,
        });
    }

    Ok(AudioPlan { tracks: planned })
}

impl AudioPlan {
    pub fn summary_lines(&self) -> Vec<String> {
        if self.tracks.is_empty() {
            return vec!["no audio tracks".to_string()];
        }
        self.tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let lang = t.language.as_deref().unwrap_or("und");
                let layout = t.channels.map_or("?", layout_name);
                let kind = if t.lossless { "lossless" } else { "lossy" };
                let action = match &t.action {
                    Action::Copy { preroll: 0 } => "copy".to_string(),
                    Action::Copy { preroll } => format!("copy without {preroll} priming packet(s)"),
                    Action::Pcm { codec } => format!("{codec}, no Matroska codec ID for {}", t.codec_name),
                    Action::Encode { codec, bitrate, layout, .. } => {
                        let mut s = codec_display(codec).to_string();
                        if let Some(b) = bitrate {
                            s.push_str(&format!(" {b}"));
                        }
                        if let Some((name, _)) = layout.as_ref().filter(|(name, _)| Some(*name) != t.channels.map(layout_name)) {
                            s.push_str(&format!(" as {name}"));
                        }
                        s
                    }
                };
                format!("track {i}: {lang} {} {layout} ({kind}) -> {action}", t.codec_name)
            })
            .collect()
    }
}

pub fn extract(
    source_file: &Path,
    tracks_path: &Path,
    plan: &AudioPlan,
    subtitles: &[(usize, Option<&'static str>)],
) -> Result<()> {
    if plan.tracks.is_empty() && subtitles.is_empty() {
        // The muxer only tests whether this file exists.
        let _ = std::fs::remove_file(tracks_path);
        return Ok(());
    }

    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(source_file);

    for t in &plan.tracks {
        cmd.args(["-map", &format!("0:a:{}", t.audio_index)]);
    }
    for (index, _) in subtitles {
        cmd.args(["-map", &format!("0:s:{index}")]);
    }
    for (out_idx, (_, codec)) in subtitles.iter().enumerate() {
        cmd.args([format!("-c:s:{out_idx}"), codec.unwrap_or("copy").to_string()]);
    }

    for (out_idx, t) in plan.tracks.iter().enumerate() {
        match &t.action {
            Action::Copy { preroll } => {
                cmd.args([format!("-c:a:{out_idx}"), "copy".into()]);
                if *preroll > 0 {
                    cmd.args([format!("-bsf:a:{out_idx}"), format!("noise=drop=lt(n\\,{preroll})")]);
                }
            }
            Action::Pcm { codec } => {
                cmd.args([format!("-c:a:{out_idx}"), (*codec).to_string()]);
            }
            Action::Encode { codec, bitrate, options, layout } => {
                if let Some((name, filter)) = layout {
                    cmd.args([format!("-filter:a:{out_idx}"), filter.clone()]);
                    // libopusenc's default channel order swaps channels of 5.0 and 6.1.
                    if OPUS_LAYOUTS.iter().any(|(n, c)| n == name && c.len() > 2) {
                        cmd.args([format!("-mapping_family:a:{out_idx}"), "1".into()]);
                    }
                }
                cmd.args([format!("-c:a:{out_idx}"), codec.clone()]);
                // ffmpeg's FLAC encoder cuts deeper samples to 24 bits unless allowed experimental ones.
                if codec == "flac" && t.bits_per_raw_sample > 24 {
                    cmd.args([format!("-strict:a:{out_idx}"), "experimental".into()]);
                }
                if let Some(b) = bitrate {
                    cmd.args([format!("-b:a:{out_idx}"), b.clone()]);
                }
                for (k, v) in options {
                    cmd.args([format!("-{k}:a:{out_idx}"), v.clone()]);
                }
                let marker = codec_display(codec);
                let name = match t.title.as_deref().map(str::trim) {
                    // Or re-encoding stacks markers: "Deutsch (Opus) (Opus)".
                    Some(title) if !title.is_empty() => {
                        let base = strip_codec_marker(title, marker);
                        if base.is_empty() { marker.to_string() } else { format!("{base} ({marker})") }
                    }
                    _ => marker.to_string(),
                };
                cmd.args([format!("-metadata:s:a:{out_idx}"), format!("title={name}")]);
            }
        }
    }

    // Any hand-set disposition keeps ffmpeg, passthrough its muxer, from flagging a first track default.
    cmd.args(["-disposition:0", "-attached_pic", "-default_mode", "passthrough"]);
    cmd.arg(tracks_path);

    // Transcoding every kept track, so it scales with the runtime of the file.
    let out = crate::ext::output_with_timeout(&mut cmd, 7200, "ffmpeg track extraction")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg track extraction failed:\n{stderr}");
    }

    Ok(())
}

pub fn has_chapters(path: &Path) -> Result<bool> {
    #[derive(Deserialize)]
    struct Chapters { #[serde(default)] chapters: Vec<serde_json::Value> }
    let probe: Chapters = crate::ext::ffprobe_json(&["-v", "error", "-show_chapters", "-of", "json"], path)?;
    Ok(!probe.chapters.is_empty())
}

pub fn mux_final(
    video_path: &Path,
    video_args: &[String],
    timestamps: Option<&Path>,
    tracks_path: &Path,
    source_file: &Path,
    source_shift_ms: i64,
    source_subtitles: &[u64],
    output_path: &Path,
) -> Result<()> {
    let has_tracks = std::fs::metadata(tracks_path).is_ok_and(|m| m.len() > 0);

    // video.ivf has none; in copy mode the source has its own and track 0 may be audio.
    let video_disps = if video_path.extension().is_some_and(|e| e == "ivf") {
        probe_dispositions(source_file, "v")
    } else {
        Vec::new()
    };

    let mut cmd = Command::new(external_bin("mkvmerge"));
    cmd.arg("-o").arg(output_path);

    if let Some(d) = video_disps.first() {
        cmd.args(d.to_mkvmerge_flags(0));
    }
    // In copy mode the video comes from the source, whose attachments follow once more below.
    cmd.args(["--no-audio", "--no-subtitles", "--no-chapters", "--no-attachments",
              "--no-global-tags", "--no-track-tags"]);
    if let Some(ts) = timestamps {
        let mut spec = std::ffi::OsString::from("0:");
        spec.push(ts);
        cmd.arg("--timestamps").arg(spec);
    }
    cmd.args(video_args);
    cmd.arg(video_path);

    if has_tracks {
        cmd.args(["--no-video", "--no-chapters", "--no-global-tags", "--no-track-tags"]);
        cmd.arg(tracks_path);
    }

    cmd.args(["--no-video", "--no-audio", "--no-global-tags", "--no-track-tags"]);
    if source_subtitles.is_empty() {
        cmd.arg("--no-subtitles");
    } else {
        let ids = source_subtitles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        cmd.args(["--subtitle-tracks", &ids]);
    }
    if source_shift_ms != 0 {
        if !source_subtitles.is_empty() {
            cmd.arg("--sync").arg(format!("-1:{source_shift_ms}"));
        }
        if has_chapters(source_file)? {
            cmd.arg("--chapter-sync").arg(source_shift_ms.to_string());
        }
    }
    cmd.arg(source_file);

    let out = crate::ext::output_with_timeout(&mut cmd, 3600, "mkvmerge")?;
    // mkvmerge exits 1 for warnings (non-fatal), 2+ for errors
    if out.status.code().unwrap_or(2) >= 2 {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("mkvmerge failed:\n{stderr}");
    }
    // Exit 1 also covers "track skipped: unsupported codec", which silently drops a track.
    if out.status.code() == Some(1) {
        let msg = String::from_utf8_lossy(&out.stdout);
        let warnings: Vec<&str> = msg.lines().filter(|l| l.contains("Warning")).collect();
        if !warnings.is_empty() {
            tracing::warn!("mkvmerge: {}", warnings.join(" | "));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn sample_set() -> HashSet<String> {
        ["truehd", "flac", "alac", "dts", "wavpack"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn lossless_by_codec_name() {
        let set = sample_set();
        assert!(is_lossless("truehd", None, &set));
        assert!(is_lossless("flac", None, &set));
        assert!(is_lossless("pcm_s24le", None, &set)); // pcm always lossless
        assert!(!is_lossless("eac3", None, &set));
        assert!(!is_lossless("aac", None, &set));
    }

    #[test]
    fn dts_lossless_only_for_master_audio() {
        let set = sample_set();
        assert!(is_lossless("dts", Some("DTS-HD MA"), &set));
        assert!(!is_lossless("dts", Some("DTS-HD HRA"), &set));
        assert!(!is_lossless("dts", Some("DTS"), &set));
        assert!(!is_lossless("dts", None, &set));
    }

    #[test]
    fn parse_codecs_picks_audio_lossless() {
        let sample = "\
 DEA..S flac    FLAC (Free Lossless Audio Codec)
 DEA.L. aac     AAC (Advanced Audio Coding)
 DEAI.S truehd  TrueHD
 DEV..S ffv1    FFV1 (video, lossless)
 D.A.LS dts     DCA (DTS Coherent Acoustics)
";
        let set = parse_lossless_codecs(sample);
        assert!(set.contains("flac"));
        assert!(set.contains("truehd"));
        assert!(set.contains("dts"));
        assert!(!set.contains("aac"));
        assert!(!set.contains("ffv1"));
    }

    #[test]
    fn every_source_channel_gets_a_place_in_the_opus_layout() {
        let target = |layout: &str, channels: u32| opus_layout(Some(layout), Some(channels));
        assert_eq!(target("stereo", 2), ("stereo", "aformat=channel_layouts=stereo".to_string()));
        assert_eq!(target("5.1(side)", 6).1, "channelmap=map=FL-FL|FR-FR|FC-FC|LFE-LFE|SL-BL|SR-BR,aformat=channel_layouts=5.1");
        assert_eq!(target("5.0", 5).0, "5.0");
        assert_eq!(target("6.1", 7).0, "6.1");
        assert_eq!(target("7.1", 8).0, "7.1");
        assert_eq!(target("2.1", 3), ("5.1", "aformat=channel_layouts=5.1".to_string()));
        assert_eq!(target("4.0", 4).0, "6.1");
        assert_eq!(target("quad(side)", 4).1, "channelmap=map=FL-FL|FR-FR|SL-BL|SR-BR,aformat=channel_layouts=quad");
        assert_eq!(target("hexagonal", 6).1, "channelmap=map=FL-FL|FR-FR|FC-FC|BL-SL|BR-SR|BC-BC,aformat=channel_layouts=6.1");
        assert_eq!(target("6.1(back)", 7).0, "6.1");
        assert_eq!(target("FL+FR+LFE", 3).0, "5.1");
        assert_eq!(target("downmix", 2).1, "channelmap=map=DL-FL|DR-FR,aformat=channel_layouts=stereo");
    }

    #[test]
    fn a_layout_without_a_place_for_every_channel_keeps_the_channel_count() {
        assert_eq!(opus_layout(Some("7.1(wide)"), Some(8)), ("7.1", "aformat=channel_layouts=7.1".to_string()));
        assert_eq!(opus_layout(Some("octagonal"), Some(8)).0, "7.1");
        assert_eq!(opus_layout(Some("6.0(front)"), Some(6)).0, "5.1");
        assert_eq!(opus_layout(Some("6 channels"), Some(6)).0, "5.1");
        assert_eq!(opus_layout(None, Some(1)).0, "mono");
        assert_eq!(opus_layout(Some("7.1.4"), Some(12)).0, "7.1");
    }

    #[test]
    fn only_packets_mostly_inside_the_skipped_samples_are_dropped() {
        let aac = |n: usize| std::iter::repeat_n(Some(1024.0), n);
        assert_eq!(whole_packets_within(1024.0, aac(10)), 1);
        assert_eq!(whole_packets_within(19136.0, aac(40)), 19);
        assert_eq!(whole_packets_within(18900.0, aac(40)), 18);
        assert_eq!(whole_packets_within(300.0, aac(40)), 0);
        assert_eq!(whole_packets_within(5000.0, [Some(1024.0), None, Some(1024.0)].into_iter()), 1);
    }

    #[test]
    fn samples_without_a_matroska_codec_id_keep_their_depth() {
        let track = |fmt: &str, bits: u32| AudioTrack {
            audio_index: 0, codec_name: "pcm_bluray".into(), profile: None, channels: Some(2),
            channel_layout: None, sample_rate: 48000, bits_per_raw_sample: bits,
            sample_fmt: fmt.into(), initial_padding: 0, language: None, title: None,
        };
        assert_eq!(pcm_codec(&track("s16", 16)), "pcm_s16le");
        assert_eq!(pcm_codec(&track("s16", 0)), "pcm_s16le");
        assert_eq!(pcm_codec(&track("s32", 20)), "pcm_s24le");
        assert_eq!(pcm_codec(&track("s32", 24)), "pcm_s24le");
        assert_eq!(pcm_codec(&track("s32", 0)), "pcm_s32le");
        assert_eq!(pcm_codec(&track("fltp", 0)), "pcm_f32le");
    }

    #[test]
    fn codec_marker_is_not_stacked_on_reencode() {
        assert_eq!(strip_codec_marker("Deutsch DD 5.1 (Opus)", "Opus"), "Deutsch DD 5.1");
        assert_eq!(strip_codec_marker("Deutsch DD 5.1", "Opus"), "Deutsch DD 5.1");
        assert_eq!(strip_codec_marker("Kommentar (Regie)", "Opus"), "Kommentar (Regie)");
        assert_eq!(strip_codec_marker("(Opus)", "Opus"), "");
    }
}
