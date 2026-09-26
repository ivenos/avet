use anyhow::{Context, Result};
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
    ("cube", "FL+FR+BL+BR+TFL+TFR+TBL+TBR"), ("5.1.4", "FL+FR+FC+LFE+SL+SR+TFL+TFR+TBL+TBR"),
    ("7.1.2", "FL+FR+FC+LFE+BL+BR+SL+SR+TFL+TFR"), ("7.1.4", "FL+FR+FC+LFE+BL+BR+SL+SR+TFL+TFR+TBL+TBR"),
    ("7.2.3", "FL+FR+FC+LFE+BL+BR+SL+SR+TFL+TFR+TBC+LFE2"),
    ("9.1.4", "FL+FR+FC+LFE+BL+BR+FLC+FRC+SL+SR+TFL+TFR+TBL+TBR"),
    ("9.1.6", "FL+FR+FC+LFE+BL+BR+FLC+FRC+SL+SR+TFL+TFR+TBL+TBR+TSL+TSR"),
    ("hexadecagonal", "FL+FR+FC+BL+BR+BC+SL+SR+TFL+TFC+TFR+TBL+TBC+TBR+WL+WR"),
    ("binaural", "BIL+BIR"), ("downmix", "DL+DR"),
    ("22.2", "FL+FR+FC+LFE+BL+BR+FLC+FRC+BC+SL+SR+TC+TFL+TFC+TFR+TBL+TBC+TBR+LFE2+TSL+TSR+BFC+BFL+BFR"),
];

/// Channels swresample drops from a downmix, and where each goes instead, by preference.
const FOLDS: &[(&str, &[&[&str]])] = &[
    ("TBL", &[&["BL"], &["SL"], &["FL"]]),
    ("TBR", &[&["BR"], &["SR"], &["FR"]]),
    ("TBC", &[&["BC"], &["BL", "BR"], &["SL", "SR"], &["FL", "FR"]]),
    ("TSL", &[&["SL"], &["BL"], &["FL"]]),
    ("TSR", &[&["SR"], &["BR"], &["FR"]]),
    ("TC", &[&["FC"], &["FL", "FR"]]),
    ("TFC", &[&["FC"], &["FL", "FR"]]),
    ("LFE2", &[&["LFE"]]),
    ("WL", &[&["FL"]]),
    ("WR", &[&["FR"]]),
    ("SDL", &[&["SL"], &["BL"], &["FL"]]),
    ("SDR", &[&["SR"], &["BR"], &["FR"]]),
    ("BFC", &[&["FC"], &["FL", "FR"]]),
    ("BFL", &[&["FL"]]),
    ("BFR", &[&["FR"]]),
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

struct AudioCodecs {
    lossless: HashSet<String>,
    decodable: Option<HashSet<String>>,
}

static AUDIO_CODECS: OnceLock<AudioCodecs> = OnceLock::new();

fn audio_codecs() -> &'static AudioCodecs {
    static FALLBACK: OnceLock<AudioCodecs> = OnceLock::new();
    if let Some(codecs) = AUDIO_CODECS.get() {
        return codecs;
    }
    match probe_audio_codecs() {
        Ok(codecs) => AUDIO_CODECS.get_or_init(|| codecs),
        Err(e) => {
            tracing::warn!("ffmpeg -codecs query failed ({e:#}); using built-in lossless list");
            FALLBACK.get_or_init(|| AudioCodecs { lossless: fallback_lossless_codecs(), decodable: None })
        }
    }
}

fn probe_audio_codecs() -> Result<AudioCodecs> {
    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-codecs"]);
    let out = crate::ext::output_with_timeout(&mut cmd, 60, "ffmpeg -codecs")?;
    if !out.status.success() {
        return Err(crate::ext::tool_error("ffmpeg -codecs", out.status, &String::from_utf8_lossy(&out.stderr)));
    }
    Ok(parse_audio_codecs(&String::from_utf8_lossy(&out.stdout)))
}

/// Flag 1 = D (decodes), flag 3 = A (audio), flag 6 = S (lossless) in `ffmpeg -codecs`.
fn parse_audio_codecs(stdout: &str) -> AudioCodecs {
    let (mut lossless, mut decodable) = (HashSet::new(), HashSet::new());
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let (Some(flags), Some(name)) = (fields.next(), fields.next()) else { continue };
        let f = flags.as_bytes();
        if f.len() < 6 || f[2] != b'A' {
            continue;
        }
        if f[5] == b'S' {
            lossless.insert(name.to_string());
        }
        if f[0] == b'D' {
            decodable.insert(name.to_string());
        }
    }
    AudioCodecs { lossless, decodable: Some(decodable) }
}

/// Audio encoder names ffmpeg offers, from the first successful `ffmpeg -encoders`.
static AUDIO_ENCODERS: OnceLock<HashSet<String>> = OnceLock::new();

fn audio_encoders() -> Option<&'static HashSet<String>> {
    if let Some(set) = AUDIO_ENCODERS.get() {
        return Some(set);
    }
    match probe_audio_encoders() {
        Ok(set) => Some(AUDIO_ENCODERS.get_or_init(|| set)),
        Err(e) => {
            tracing::warn!("ffmpeg -encoders query failed ({e:#}); codec names go unchecked");
            None
        }
    }
}

fn probe_audio_encoders() -> Result<HashSet<String>> {
    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-encoders"]);
    let out = crate::ext::output_with_timeout(&mut cmd, 60, "ffmpeg -encoders")?;
    if !out.status.success() {
        return Err(crate::ext::tool_error("ffmpeg -encoders", out.status, &String::from_utf8_lossy(&out.stderr)));
    }
    Ok(parse_audio_encoders(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_audio_encoders(stdout: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let (Some(flags), Some(name)) = (fields.next(), fields.next()) else { continue };
        // The legend rows ("A..... = Audio") carry the same flags but no encoder name.
        if flags.len() == 6
            && flags.starts_with('A')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
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

/// `stream_disposition`, not `stream=disposition`: ffprobe prints an empty object for that one.
fn probe_dispositions(path: &Path, stream_spec: &str) -> Vec<FfprobeDispStream> {
    match crate::ext::ffprobe_json::<FfprobeDispOutput>(
        &["-v", "error", "-select_streams", stream_spec,
          "-show_entries", "stream_disposition", "-of", "json"],
        path,
    ) {
        Ok(p) => p.streams,
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

/// Into a seekable sink: into a pipe the muxer cannot finish ADTS AAC's header and refuses it.
fn copies_into_matroska(source_file: &Path, audio_index: usize) -> Result<bool> {
    use std::os::unix::process::ExitStatusExt;

    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(source_file)
        .args(["-map", &format!("0:a:{audio_index}"), "-c", "copy", "-frames:a", "1", "-f", "matroska", "/dev/null"]);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "ffmpeg copy check")?;
    if out.status.signal().is_some() {
        return Err(crate::ext::tool_error("ffmpeg copy check", out.status, &String::from_utf8_lossy(&out.stderr)));
    }
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
    let layout = layout
        .split_once(" channels (")
        .and_then(|(_, names)| names.strip_suffix(')'))
        .unwrap_or(layout);
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
        "DL" | "BIL" => Some("FL"), "DR" | "BIR" => Some("FR"),
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
    let count = source.as_ref().map_or(channels.unwrap_or(2), |s| s.len() as u32).clamp(1, 8);
    let name = OPUS_LAYOUTS[count as usize - 1].0;
    let fold = source.as_deref().and_then(fold_unplaced).map(|f| f + ",").unwrap_or_default();
    (name, format!("{fold}aformat=channel_layouts={name}"))
}

/// In float, so the sums cannot clip before aformat's normalized downmix.
fn fold_unplaced(source: &[&str]) -> Option<String> {
    let folds = |c: &str| FOLDS.iter().find(|(f, _)| *f == c).map(|(_, places)| *places);
    let mut rows: Vec<(&str, Vec<&str>)> = source.iter().filter(|c| folds(c).is_none()).map(|c| (*c, Vec::new())).collect();
    let mut folded = false;
    for &c in source {
        let Some(places) = folds(c) else { continue };
        match places.iter().find(|p| p.iter().all(|d| rows.iter().any(|(k, _)| k == d))) {
            Some(place) => {
                for d in *place {
                    if let Some(row) = rows.iter_mut().find(|(k, _)| k == d) {
                        row.1.push(c);
                    }
                }
                folded = true;
            }
            None => rows.push((c, Vec::new())),
        }
    }
    if !folded {
        return None;
    }
    let layout = rows.iter().map(|(k, _)| *k).collect::<Vec<_>>().join("+");
    let gains = rows
        .iter()
        .map(|(k, extra)| format!("{k}={k}{}", extra.iter().map(|x| format!("+0.7071*{x}")).collect::<String>()))
        .collect::<Vec<_>>()
        .join("|");
    Some(format!("aformat=sample_fmts=fltp,pan={layout}|{gains}"))
}

/// Drops a trailing "(Marker)" this function added on an earlier run.
fn strip_codec_marker<'a>(title: &'a str, marker: &str) -> &'a str {
    title
        .strip_suffix(')')
        .and_then(|t| t.strip_suffix(marker))
        .and_then(|t| t.strip_suffix('('))
        .map_or(title, str::trim_end)
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

pub struct AudioPlan {
    tracks: Vec<PlannedTrack>,
}

/// A rule keyed by a name ffprobe never reports, such as `ac-3`, leaves its tracks on the default.
fn warn_about_unknown_codec_rules(config: &AudioConfig, decodable: Option<&HashSet<String>>) {
    let Some(decodable) = decodable else { return };
    for key in config.codec_rules.keys().filter(|k| !decodable.contains(*k)) {
        tracing::warn!("audio.codec_rules.{key} is not a codec name ffmpeg knows, so it matches no track");
    }
}

pub fn plan(source_file: &Path, config: &AudioConfig) -> Result<AudioPlan> {
    let tracks = probe_audio_tracks(source_file).context("probe audio tracks")?;
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

    let carried = tracks.len();
    let kept: Vec<AudioTrack> = tracks
        .into_iter()
        .filter(|t| crate::config::language_selected(&config.language_whitelist, t.language.as_deref()))
        .collect();

    if kept.is_empty() {
        if carried > 0 {
            tracing::warn!(
                "no audio tracks match language whitelist {:?} - audio omitted",
                config.language_whitelist
            );
        }
        return Ok(AudioPlan { tracks: vec![] });
    }

    let codecs = audio_codecs();
    warn_about_unknown_codec_rules(config, codecs.decodable.as_ref());
    let mut planned = Vec::with_capacity(kept.len());
    for track in kept {
        let lossless = is_lossless(&track.codec_name, track.profile.as_deref(), &codecs.lossless);
        let decodable = codecs.decodable.as_ref().is_none_or(|d| d.contains(&track.codec_name));
        let r = config.resolve(&track.codec_name, lossless);
        let action = if r.mode == AudioMode::Copy || !decodable {
            if copies_into_matroska(source_file, track.audio_index)? {
                if r.mode == AudioMode::Encode {
                    tracing::warn!("audio track {} ({}): ffmpeg cannot decode it - copied instead", track.audio_index, track.codec_name);
                }
                let preroll = preroll_packets(source_file, &track)
                    .with_context(|| format!("probe the priming of audio track {}", track.audio_index))?;
                Action::Copy { preroll }
            } else if decodable {
                Action::Pcm { codec: pcm_codec(&track) }
            } else {
                tracing::warn!(
                    "audio track {} ({}): ffmpeg can neither decode it nor copy it into Matroska - skipped",
                    track.audio_index, track.codec_name
                );
                continue;
            }
        } else {
            let codec = r.codec.ok_or_else(|| anyhow::anyhow!(
                "audio track {}: codec is required when mode = encode", track.audio_index
            ))?;
            if audio_encoders().is_some_and(|known| !known.contains(codec)) {
                return Err(anyhow::Error::new(crate::job::Transient).context(format!(
                    "audio track {}: ffmpeg has no encoder '{codec}'", track.audio_index
                )));
            }
            let layout = codec
                .contains("opus")
                .then(|| opus_layout(track.channel_layout.as_deref(), track.channels));
            let bitrate = if output_is_lossless(codec) {
                None
            } else {
                let b = r.bitrate.and_then(|b| match &layout {
                    Some((name, _)) => b.resolve_layout(name),
                    None => b.resolve(track.channels),
                }).map(str::to_owned);
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
            Action::Encode { codec: codec.to_owned(), bitrate, options, layout }
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
            Action::Encode { .. } => encode_args(&mut cmd, out_idx, t),
        }
    }

    // Any hand-set disposition keeps ffmpeg, passthrough its muxer, from flagging a first track default.
    cmd.args(["-disposition:0", "-attached_pic", "-default_mode", "passthrough"]);
    cmd.arg(tracks_path);

    let out = crate::ext::output_with_timeout(
        &mut cmd, crate::ext::whole_file_timeout(source_file, 7200), "ffmpeg track extraction",
    )?;
    if !out.status.success() {
        return Err(extraction_error(out.status, &String::from_utf8_lossy(&out.stderr)));
    }

    Ok(())
}

fn encode_args(cmd: &mut Command, out_idx: usize, t: &PlannedTrack) {
    let Action::Encode { codec, bitrate, options, layout } = &t.action else { return };
    if let Some((name, filter)) = layout {
        cmd.args([format!("-filter:a:{out_idx}"), filter.clone()]);
        // libopusenc's default channel order swaps channels of 5.0 and 6.1.
        if OPUS_LAYOUTS.iter().any(|(n, c)| n == name && c.len() > 2) {
            cmd.args([format!("-mapping_family:a:{out_idx}"), "1".into()]);
        }
    }
    cmd.args([format!("-c:a:{out_idx}"), codec.clone()]);
    // libopus takes 8 to 48 kHz, and ffmpeg picks the nearest: 32 kHz would become 24.
    if codec.contains("opus") {
        cmd.args([format!("-ar:a:{out_idx}"), "48000".into()]);
    }
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

/// Opens every encoder on the first second: a refused codec or option fails before the video encode.
pub fn check_encoders(source_file: &Path, plan: &AudioPlan) -> Result<()> {
    let tracks: Vec<&PlannedTrack> = plan.tracks.iter().filter(|t| matches!(t.action, Action::Encode { .. })).collect();
    if tracks.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-t", "1", "-i"]).arg(source_file);
    for t in &tracks {
        cmd.args(["-map", &format!("0:a:{}", t.audio_index)]);
    }
    for (out_idx, t) in tracks.iter().enumerate() {
        encode_args(&mut cmd, out_idx, t);
    }
    cmd.args(["-f", "null", "-"]);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "ffmpeg audio encoder check")?;
    if !out.status.success() {
        return Err(extraction_error(out.status, &String::from_utf8_lossy(&out.stderr))
            .context("try the audio encoders on the first second"));
    }
    Ok(())
}

fn extraction_error(status: std::process::ExitStatus, stderr: &str) -> anyhow::Error {
    let err = crate::ext::tool_error("ffmpeg track extraction", status, stderr);
    let profile_errors = ["Option not found", "Error applying encoder options", "experimental codecs are not enabled"];
    if profile_errors.iter().any(|m| stderr.contains(m)) {
        err.context(crate::job::Transient)
    } else {
        err
    }
}

/// BCP 47 tags by kind and ffprobe index; ffmpeg keeps ISO 639-2 only, pt-BR becomes por.
#[derive(Default)]
pub struct Languages {
    /// As mkvmerge read it: `--language` refuses codes it does not know, such as `english`.
    video: Option<String>,
    audio: Vec<Option<String>>,
    subtitles: Vec<Option<String>>,
}

impl Languages {
    pub fn probe(source: &Path) -> Result<Self> {
        match Self::read(source) {
            Err(e) if crate::job::is_transient(&e) => Err(e),
            Err(e) => {
                tracing::warn!("could not read the BCP 47 language tags of {}: {e:#}", source.display());
                Ok(Self::default())
            }
            ok => ok,
        }
    }

    fn read(source: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Identify { #[serde(default)] tracks: Vec<Track> }
        #[derive(Deserialize)]
        struct Track { #[serde(rename = "type")] kind: String, #[serde(default)] properties: Props }
        #[derive(Deserialize, Default)]
        struct Props { language: Option<String>, language_ietf: Option<String> }
        #[derive(Deserialize)]
        struct Probe { #[serde(default)] streams: Vec<Stream> }
        #[derive(Deserialize)]
        struct Stream { #[serde(default)] codec_type: String, #[serde(default)] tags: Tags }
        #[derive(Deserialize, Default)]
        struct Tags { language: Option<String> }

        let mut cmd = Command::new(external_bin("mkvmerge"));
        cmd.args(["--identify", "--identification-format", "json"]).arg(source);
        let out = crate::ext::output_with_timeout(&mut cmd, 300, "mkvmerge --identify")?;
        if out.status.code().unwrap_or(2) >= 2 {
            return Err(crate::ext::tool_error("mkvmerge identify", out.status, &String::from_utf8_lossy(&out.stdout)));
        }
        let identify: Identify = serde_json::from_slice(&out.stdout).context("parse mkvmerge identify output")?;
        let probe: Probe = crate::ext::ffprobe_json(
            &["-v", "error", "-show_entries", "stream=codec_type:stream_tags=language", "-of", "json"],
            source,
        )?;

        let tags = |ffprobe_kind: &str, mkvmerge_kind: &str| {
            let ours: Vec<Option<&str>> = probe.streams.iter()
                .filter(|s| s.codec_type == ffprobe_kind)
                .map(|s| s.tags.language.as_deref())
                .collect();
            let theirs: Vec<&Props> = identify.tracks.iter()
                .filter(|t| t.kind == mkvmerge_kind)
                .map(|t| &t.properties)
                .collect();
            matched_tags(&ours, &theirs.iter().map(|p| (p.language.as_deref(), p.language_ietf.as_deref())).collect::<Vec<_>>())
        };
        Ok(Self {
            video: identify.tracks.iter()
                .find(|t| t.kind == "video")
                .and_then(|t| t.properties.language_ietf.clone().or_else(|| t.properties.language.clone()))
                .filter(|l| !l.is_empty() && l != "und"),
            audio: tags("audio", "audio"),
            subtitles: tags("subtitle", "subtitles"),
        })
    }

    pub fn video(&self) -> Option<&str> {
        self.video.as_deref()
    }

    /// One per track of tracks.mkv, in the order `extract` writes them.
    pub fn of_tracks(&self, plan: &AudioPlan, subtitles: &[(usize, Option<&'static str>)]) -> Vec<Option<String>> {
        plan.tracks.iter()
            .map(|t| self.audio.get(t.audio_index).cloned().flatten())
            .chain(subtitles.iter().map(|(i, _)| self.subtitles.get(*i).cloned().flatten()))
            .collect()
    }
}

/// By position, where both tools read the same ISO 639-2 code; a TS descriptor's `ger,eng` as `ger`.
fn matched_tags(ffprobe: &[Option<&str>], mkvmerge: &[(Option<&str>, Option<&str>)]) -> Vec<Option<String>> {
    if ffprobe.len() != mkvmerge.len() {
        return vec![None; ffprobe.len()];
    }
    ffprobe.iter().zip(mkvmerge)
        .map(|(ours, (theirs, tag))| {
            let ours = (*ours)?;
            let first = ours.split(',').next()?.trim();
            if Some(first) != *theirs {
                return None;
            }
            (*tag).filter(|t| t.contains('-'))
                .or((first != ours).then_some(first))
                .map(str::to_owned)
        })
        .collect()
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
    tracks_languages: &[Option<String>],
    source_file: &Path,
    video_language: Option<&str>,
    source_shift_ms: i64,
    source_subtitles: &[u64],
    output_path: &Path,
) -> Result<()> {
    let has_tracks = std::fs::metadata(tracks_path).is_ok_and(|m| m.len() > 0);

    // video.ivf has none; in copy mode the source has its own and track 0 may be audio.
    let encoded = video_path.extension().is_some_and(|e| e == "ivf");
    let source_video = if encoded { probe_dispositions(source_file, "v:0") } else { Vec::new() };

    let mut cmd = Command::new(external_bin("mkvmerge"));
    cmd.arg("-o").arg(output_path);

    if let Some(v) = source_video.first() {
        cmd.args(v.disposition.to_mkvmerge_flags(0));
        if let Some(lang) = video_language {
            cmd.args(["--language".to_string(), format!("0:{lang}")]);
        }
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
        for (tid, lang) in tracks_languages.iter().enumerate() {
            if let Some(lang) = lang {
                cmd.arg("--language").arg(format!("{tid}:{lang}"));
            }
        }
        cmd.arg(tracks_path);
    }

    cmd.args(["--no-video", "--no-audio", "--no-track-tags"]);
    if source_subtitles.is_empty() {
        cmd.arg("--no-subtitles");
    } else {
        let ids = source_subtitles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        cmd.args(["--subtitle-tracks", &ids]);
    }
    // A copied video keeps mkvmerge's timeline, and realign_copied_video moves these with it.
    if !source_subtitles.is_empty() && encoded {
        let shift = source_shift_ms + wrapped_ts_shift_ms(source_file, source_subtitles, output_path)?;
        if shift != 0 {
            cmd.arg("--sync").arg(format!("-1:{shift}"));
        }
    }
    if source_shift_ms != 0 && has_chapters(source_file)? {
        cmd.arg("--chapter-sync").arg(source_shift_ms.to_string());
    }
    cmd.arg(source_file);

    let out = crate::ext::output_with_timeout(&mut cmd, crate::ext::whole_file_timeout(source_file, 3600), "mkvmerge")?;
    // mkvmerge exits 1 for warnings (non-fatal), 2+ for errors
    if out.status.code().unwrap_or(2) >= 2 {
        return Err(crate::ext::tool_error("mkvmerge", out.status, &String::from_utf8_lossy(&out.stdout)));
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

/// mkvmerge keeps the absolute time of a wrapping MPEG-TS, and its subtitles land past the end.
fn wrapped_ts_shift_ms(source: &Path, subtitle_ids: &[u64], scratch: &Path) -> Result<i64> {
    #[derive(Deserialize)]
    struct Probe { format: Format }
    #[derive(Deserialize)]
    struct Format { #[serde(default)] format_name: String, start_time: Option<String>, duration: Option<String> }
    #[derive(Deserialize)]
    struct Packets { #[serde(default)] packets: Vec<Packet> }
    #[derive(Deserialize)]
    struct Packet { pts_time: Option<String> }

    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-show_entries", "format=format_name,start_time,duration", "-of", "json"],
        source,
    )?;
    let secs = |s: Option<&str>| s.and_then(|v| v.parse::<f64>().ok());
    let (Some(start), Some(duration)) = (secs(probe.format.start_time.as_deref()), secs(probe.format.duration.as_deref())) else {
        return Ok(0);
    };
    if probe.format.format_name != "mpegts" {
        return Ok(0);
    }

    let subtitles = scratch.with_file_name("subtitles.mkv");
    let ids = subtitle_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let mut cmd = Command::new(external_bin("mkvmerge"));
    cmd.arg("-o").arg(&subtitles)
        .args(["--no-video", "--no-audio", "--no-attachments", "--no-chapters", "--no-global-tags",
               "--no-track-tags", "--subtitle-tracks", &ids])
        .arg(source);
    let out = crate::ext::output_with_timeout(&mut cmd, crate::ext::whole_file_timeout(source, 3600), "mkvmerge")?;
    if out.status.code().unwrap_or(2) >= 2 {
        let _ = std::fs::remove_file(&subtitles);
        return Err(crate::ext::tool_error("mkvmerge subtitle probe", out.status, &String::from_utf8_lossy(&out.stdout)));
    }
    let packets: Result<Packets> = crate::ext::ffprobe_json(
        &["-v", "error", "-read_intervals", "%+#8", "-show_entries", "packet=pts_time", "-of", "json"],
        &subtitles,
    );
    let _ = std::fs::remove_file(&subtitles);
    let first = packets?.packets.iter().filter_map(|p| secs(p.pts_time.as_deref())).reduce(f64::min);
    Ok(absolute_time_shift_ms(first, start, duration))
}

fn absolute_time_shift_ms(first_subtitle: Option<f64>, start: f64, duration: f64) -> i64 {
    match first_subtitle {
        Some(t) if t > duration + 1.0 => -(start * 1000.0).round() as i64,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_subtitle_past_the_end_of_the_file_is_moved_back_by_the_start() {
        assert_eq!(absolute_time_shift_ms(Some(95389.385), 95379.39, 70.0), -95379390);
        assert_eq!(absolute_time_shift_ms(Some(10.01), 95379.39, 70.0), 0);
        assert_eq!(absolute_time_shift_ms(None, 95379.39, 70.0), 0);
    }

    #[test]
    fn a_bcp_47_tag_is_kept_only_where_both_tools_agree_on_the_track() {
        let theirs = [(Some("por"), Some("pt-BR")), (Some("por"), Some("pt-PT")), (Some("eng"), Some("en"))];
        assert_eq!(
            matched_tags(&[Some("por"), Some("por"), Some("eng")], &theirs),
            [Some("pt-BR".to_string()), Some("pt-PT".to_string()), None]
        );
        assert_eq!(matched_tags(&[Some("ger"), Some("por"), Some("eng")], &theirs), [None, Some("pt-PT".to_string()), None]);
        assert_eq!(matched_tags(&[None, Some("por"), Some("eng")], &theirs)[0], None);
        assert_eq!(matched_tags(&[Some("por"), Some("por")], &theirs), [None, None]);

        let ts = [(Some("ger"), Some("de")), (Some("ger"), Some("de")), (Some("eng"), Some("en"))];
        assert_eq!(
            matched_tags(&[Some("ger,eng"), Some("ger"), Some("fre,eng")], &ts),
            [Some("ger".to_string()), None, None]
        );
    }

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
    fn parse_codecs_picks_audio_lossless_and_decodable() {
        let sample = "\
 D..... = Decoding supported
 ..A... = Audio codec
 DEA..S flac    FLAC (Free Lossless Audio Codec)
 DEA.L. aac     AAC (Advanced Audio Coding)
 DEAI.S truehd  TrueHD
 DEV..S ffv1    FFV1 (video, lossless)
 D.A.LS dts     DCA (DTS Coherent Acoustics)
 ..A.L. ac4     AC-4
";
        let codecs = parse_audio_codecs(sample);
        let set = &codecs.lossless;
        assert!(set.contains("flac"));
        assert!(set.contains("truehd"));
        assert!(set.contains("dts"));
        assert!(!set.contains("aac"));
        assert!(!set.contains("ffv1"));

        let decodable = codecs.decodable.unwrap();
        assert!(decodable.contains("aac") && decodable.contains("dts"));
        assert!(!decodable.contains("ac4") && !decodable.contains("ffv1") && !decodable.contains("="));
    }

    #[test]
    fn the_encoder_list_holds_audio_encoders_and_no_legend_rows() {
        let stdout = concat!(
            "Encoders:\n",
            " V..... = Video\n",
            " A..... = Audio\n",
            " ------\n",
            " V....D libx264              libx264 H.264\n",
            " A....D libopus              libopus Opus (codec opus)\n",
            " A....D flac                 FLAC (Free Lossless Audio Codec)\n",
        );
        let set = parse_audio_encoders(stdout);
        assert!(set.contains("libopus") && set.contains("flac"));
        assert!(!set.contains("libx264"));
        assert!(!set.contains("=") && !set.contains("libopu"));
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
        assert_eq!(target("4 channels (FL+FR+LFE+BC)", 4), ("6.1", "aformat=channel_layouts=6.1".to_string()));
        assert_eq!(target("2 channels (FC+LFE)", 2).0, "5.1");
        assert_eq!(target("5 channels (FL+FR+LFE+SL+SR)", 5).1, "channelmap=map=FL-FL|FR-FR|LFE-LFE|SL-BL|SR-BR,aformat=channel_layouts=5.1");
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
    fn channels_swresample_would_drop_are_mixed_into_their_place_first() {
        assert_eq!(
            opus_layout(Some("7.1.4"), Some(12)).1,
            "aformat=sample_fmts=fltp,pan=FL+FR+FC+LFE+BL+BR+SL+SR+TFL+TFR|FL=FL|FR=FR|FC=FC|LFE=LFE\
             |BL=BL+0.7071*TBL|BR=BR+0.7071*TBR|SL=SL|SR=SR|TFL=TFL|TFR=TFR,aformat=channel_layouts=7.1"
        );
        let seven_two_three = opus_layout(Some("7.2.3"), Some(12)).1;
        assert!(seven_two_three.contains("|LFE=LFE+0.7071*LFE2|"), "{seven_two_three}");
        assert!(seven_two_three.contains("|BL=BL+0.7071*TBC|BR=BR+0.7071*TBC|"), "{seven_two_three}");
        assert!(opus_layout(Some("5.1.4"), Some(10)).1.contains("|SL=SL+0.7071*TBL|SR=SR+0.7071*TBR|"));

        assert_eq!(opus_layout(Some("7.1(wide)"), Some(8)).1, "aformat=channel_layouts=7.1");
        assert_eq!(opus_layout(Some("binaural"), Some(2)).1, "channelmap=map=BIL-FL|BIR-FR,aformat=channel_layouts=stereo");
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
    fn a_rejected_option_is_retried_and_a_broken_track_is_not() {
        use std::os::unix::process::ExitStatusExt;
        let failed = std::process::ExitStatus::from_raw(8 << 8);
        let transient = |e: &anyhow::Error| e.downcast_ref::<crate::job::Transient>().is_some();

        assert!(transient(&extraction_error(failed, "Unrecognized option 'compresion_level:a:0'.\nError splitting the argument list: Option not found")));
        assert!(transient(&extraction_error(failed, "[flac] Error setting option compression_level to value abc.\n[aost#0:0/flac] Error applying encoder options: Invalid argument")));
        assert!(!transient(&extraction_error(failed, "[truehd] Invalid data found when processing input")));
    }

    #[test]
    fn codec_marker_is_not_stacked_on_reencode() {
        assert_eq!(strip_codec_marker("Deutsch DD 5.1 (Opus)", "Opus"), "Deutsch DD 5.1");
        assert_eq!(strip_codec_marker("Deutsch DD 5.1", "Opus"), "Deutsch DD 5.1");
        assert_eq!(strip_codec_marker("Kommentar (Regie)", "Opus"), "Kommentar (Regie)");
        assert_eq!(strip_codec_marker("(Opus)", "Opus"), "");
    }
}
