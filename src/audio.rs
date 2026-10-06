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
    id: Option<String>,
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
    stream_id: Option<String>,
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
          "stream=id,codec_name,profile,channels,channel_layout,sample_rate,bits_per_raw_sample,sample_fmt,initial_padding:stream_tags=language,title",
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
            stream_id: s.id,
            language: s.tags.language,
            title: s.tags.title,
        })
        .collect())
}

fn source_span(source_file: &Path) -> Result<Option<(f64, f64)>> {
    #[derive(Deserialize)]
    struct Probe { #[serde(default)] format: Format }
    #[derive(Deserialize, Default)]
    struct Format { start_time: Option<String>, duration: Option<String> }

    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-show_entries", "format=start_time,duration", "-of", "json"],
        source_file,
    )
    .context("probe the source duration")?;
    let secs = |s: Option<String>| s.and_then(|s| s.parse::<f64>().ok()).filter(|s| s.is_finite());
    Ok(secs(probe.format.duration).map(|duration| (secs(probe.format.start_time).unwrap_or(0.0), duration)))
}

fn lenient<T>(probe: Result<T>, audio_index: usize) -> Result<Option<T>> {
    match probe {
        Ok(value) => Ok(Some(value)),
        Err(e) if crate::job::is_transient(&e) => Err(e),
        Err(e) => {
            tracing::warn!("audio track {audio_index}: {e:#}");
            Ok(None)
        }
    }
}

/// The widest layout of a track that changes it midway; ffmpeg opens the encoder on the first frame.
fn switched_layout(source_file: &Path, audio_index: usize, span: Option<(f64, f64)>) -> Result<Option<(String, u32)>> {
    #[derive(Deserialize)]
    struct Probe { #[serde(default)] frames: Vec<Frame> }
    #[derive(Deserialize)]
    struct Frame { channels: Option<u32>, channel_layout: Option<String> }

    const SAMPLES: u32 = 40;
    let mut intervals = vec!["%+#2".to_string()];
    if let Some((start, duration)) = span {
        let step = duration / f64::from(SAMPLES);
        intervals.extend((1..SAMPLES).map(|k| format!("{:.3}%+#2", start + f64::from(k) * step)));
    }
    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", &format!("a:{audio_index}"), "-read_intervals", &intervals.join(","),
          "-show_entries", "frame=channels,channel_layout", "-of", "json"],
        source_file,
    )
    .with_context(|| format!("probe the channel layouts of audio track {audio_index}"))?;
    let layouts: Vec<(u32, String)> = probe.frames.into_iter().filter_map(|f| Some((f.channels?, f.channel_layout?))).collect();
    let Some(widest) = layouts.iter().max_by_key(|(channels, _)| *channels) else { return Ok(None) };
    Ok(layouts.iter().any(|l| l != widest).then(|| (widest.1.clone(), widest.0)))
}

/// DVB-T2's LATM AAC, which mkvmerge turns into plain AAC.
fn mkvmerge_repacks(
    source_file: &Path,
    track: &AudioTrack,
    identified: &mut Option<Vec<crate::subtitle::Identified>>,
) -> Result<Option<u64>> {
    if track.codec_name != "aac_latm" {
        return Ok(None);
    }
    if identified.is_none() {
        *identified = Some(crate::subtitle::identify(source_file, "audio")?);
    }
    let tracks = identified.as_deref().unwrap_or_default();
    Ok(match crate::subtitle::match_track(tracks, track.audio_index, track.stream_id.as_deref())[..] {
        [one] => Some(one.id),
        _ => None,
    })
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

fn opus_bitrate(bitrate: String, layout: &str, audio_index: usize) -> String {
    let channels = OPUS_LAYOUTS.iter().find(|(n, _)| *n == layout).map_or(1, |(_, c)| c.len()) as u64;
    let max = 256_000 * channels;
    match crate::config::parse_bitrate(&bitrate) {
        Ok(bits) if bits > max => {
            tracing::warn!("audio track {audio_index}: libopus takes at most {}k for {layout} - using that instead of {bitrate}", max / 1000);
            format!("{}k", max / 1000)
        }
        _ => bitrate,
    }
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

/// Drops the trailing "(Marker)" an earlier encode added.
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
    FromSource {
        id: u64,
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
    let mut span = None;
    let mut identified = None;
    for mut track in kept {
        let lossless = is_lossless(&track.codec_name, track.profile.as_deref(), &codecs.lossless);
        let decodable = codecs.decodable.as_ref().is_none_or(|d| d.contains(&track.codec_name));
        let r = config.resolve(&track.codec_name, lossless);
        let action = if r.mode == AudioMode::Copy || !decodable {
            if let Some(id) = mkvmerge_repacks(source_file, &track, &mut identified)? {
                Action::FromSource { id }
            } else if copies_into_matroska(source_file, track.audio_index)? {
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
                anyhow::bail!("audio track {}: ffmpeg has no encoder '{codec}'", track.audio_index);
            }
            if span.is_none() {
                span = Some(lenient(source_span(source_file), track.audio_index)?.flatten());
            }
            let switched = lenient(switched_layout(source_file, track.audio_index, span.flatten()), track.audio_index)?.flatten();
            if let Some((layout, channels)) = &switched {
                tracing::info!("audio track {}: changes its channel layout midway - encoding all of it as {layout}", track.audio_index);
                track.channel_layout = Some(layout.clone());
                track.channels = Some(*channels);
            }
            let widen = switched.map(|(layout, _)| format!("aformat=channel_layouts={layout}"));
            let layout = if codec.contains("opus") {
                let (name, filter) = opus_layout(track.channel_layout.as_deref(), track.channels);
                Some((name, widen.map_or_else(|| filter.clone(), |w| format!("{w},{filter}"))))
            } else {
                widen.map(|w| (layout_name(track.channels.unwrap_or(2)), w))
            };
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
            let bitrate = match (&layout, bitrate) {
                (Some((name, _)), Some(b)) if codec == "libopus" => Some(opus_bitrate(b, name, track.audio_index)),
                (_, b) => b,
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
    fn extracted(&self) -> Vec<&PlannedTrack> {
        self.tracks.iter().filter(|t| !matches!(t.action, Action::FromSource { .. })).collect()
    }

    pub fn source_tracks(&self) -> Vec<(usize, u64)> {
        self.tracks.iter()
            .filter_map(|t| match t.action {
                Action::FromSource { id } => Some((t.audio_index, id)),
                _ => None,
            })
            .collect()
    }

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
                    Action::FromSource { .. } => "copy as AAC".to_string(),
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
    let tracks = plan.extracted();
    if tracks.is_empty() && subtitles.is_empty() {
        // The muxer only tests whether this file exists.
        let _ = std::fs::remove_file(tracks_path);
        return Ok(());
    }

    let mut cmd = Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(source_file);

    for t in &tracks {
        cmd.args(["-map", &format!("0:a:{}", t.audio_index)]);
    }
    for (index, _) in subtitles {
        cmd.args(["-map", &format!("0:s:{index}")]);
    }
    for (out_idx, (_, codec)) in subtitles.iter().enumerate() {
        cmd.args([format!("-c:s:{out_idx}"), codec.unwrap_or("copy").to_string()]);
    }

    for (out_idx, t) in tracks.iter().enumerate() {
        match &t.action {
            Action::FromSource { .. } => {}
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
        return Err(crate::ext::tool_error("ffmpeg track extraction", out.status, &String::from_utf8_lossy(&out.stderr)));
    }

    Ok(())
}

fn encode_args(cmd: &mut Command, out_idx: usize, t: &PlannedTrack) {
    let Action::Encode { codec, bitrate, options, layout } = &t.action else { return };
    if let Some((name, filter)) = layout {
        cmd.args([format!("-filter:a:{out_idx}"), filter.clone()]);
        // libopusenc's default channel order swaps channels of 5.0 and 6.1.
        if codec.contains("opus") && OPUS_LAYOUTS.iter().any(|(n, c)| n == name && c.len() > 2) {
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
        return Err(crate::ext::tool_error("ffmpeg track extraction", out.status, &String::from_utf8_lossy(&out.stderr))
            .context("try the audio encoders on the first second"));
    }
    Ok(())
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
        struct Props { number: Option<u64>, language: Option<String>, language_ietf: Option<String> }
        #[derive(Deserialize)]
        struct Probe { #[serde(default)] streams: Vec<Stream> }
        #[derive(Deserialize)]
        struct Stream { #[serde(default)] codec_type: String, id: Option<String>, #[serde(default)] tags: Tags }
        #[derive(Deserialize, Default)]
        struct Tags { language: Option<String> }

        let mut cmd = crate::ext::mkvmerge();
        cmd.args(["--identify", "--identification-format", "json"]).arg(source);
        let out = crate::ext::mkvmerge_output(&mut cmd, 300, "mkvmerge identify")?;
        let identify: Identify = serde_json::from_slice(&out.stdout).context("parse mkvmerge identify output")?;
        let probe: Probe = crate::ext::ffprobe_json(
            &["-v", "error", "-show_entries", "stream=codec_type,id:stream_tags=language", "-of", "json"],
            source,
        )?;

        let tags = |ffprobe_kind: &str, mkvmerge_kind: &str| {
            let ours: Vec<(Option<&str>, Option<&str>)> = probe.streams.iter()
                .filter(|s| s.codec_type == ffprobe_kind)
                .map(|s| (s.id.as_deref(), s.tags.language.as_deref()))
                .collect();
            let theirs: Vec<Twin> = identify.tracks.iter()
                .filter(|t| t.kind == mkvmerge_kind)
                .map(|t| Twin {
                    number: t.properties.number,
                    legacy: t.properties.language.as_deref(),
                    ietf: t.properties.language_ietf.as_deref(),
                })
                .collect();
            matched_tags(&ours, &theirs)
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
        plan.extracted().into_iter()
            .map(|t| self.audio.get(t.audio_index).cloned().flatten())
            .chain(subtitles.iter().map(|(i, _)| self.subtitles.get(*i).cloned().flatten()))
            .collect()
    }
}

struct Twin<'a> {
    number: Option<u64>,
    legacy: Option<&'a str>,
    ietf: Option<&'a str>,
}

/// Only what mkvmerge read for the same track, which `--language` cannot refuse.
fn matched_tags(ffprobe: &[(Option<&str>, Option<&str>)], mkvmerge: &[Twin]) -> Vec<Option<String>> {
    let same_tracks = ffprobe.len() == mkvmerge.len();
    ffprobe.iter().enumerate()
        .map(|(i, (id, ours))| {
            let ours = (*ours)?;
            let first = ours.split(',').next()?.trim();
            let twin = match id.and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok()) {
                Some(number) => mkvmerge.iter().find(|t| t.number == Some(number)),
                None => mkvmerge.get(i).filter(|_| same_tracks),
            }?;
            let legacy = twin.legacy?;
            if first != ours {
                return Some(twin.ietf.unwrap_or(legacy).to_owned());
            }
            twin.ietf.filter(|_| crate::config::language_matches(first, legacy)).map(str::to_owned)
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
    sources: &Sources,
    output_path: &Path,
) -> Result<Option<Vec<bool>>> {
    let has_tracks = std::fs::metadata(tracks_path).is_ok_and(|m| m.len() > 0);
    let from_source = !(sources.source_audio.is_empty() && sources.source_subtitles.is_empty());

    // video.ivf has none; in copy mode the source has its own and track 0 may be audio.
    let encoded = video_path.extension().is_some_and(|e| e == "ivf");
    let source_video = if encoded { probe_dispositions(source_file, "v:0") } else { Vec::new() };

    let mut cmd = crate::ext::mkvmerge();
    cmd.arg("-o").arg(output_path);

    // Otherwise the tracks mkvmerge takes from the source end up behind all of ffmpeg's.
    let order = if from_source {
        let videos: Vec<u64> = if encoded {
            vec![0]
        } else {
            crate::subtitle::identify(video_path, "video")?.into_iter().map(|t| t.id).collect()
        };
        let order = track_order(&videos, sources, if has_tracks { 2 } else { 1 });
        let list: Vec<String> = order.iter().map(|(file, id)| format!("{file}:{id}")).collect();
        cmd.arg("--track-order").arg(list.join(","));
        Some(order.iter().map(|(file, _)| has_tracks && *file == 1).collect())
    } else {
        None
    };

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
        let shift = extraction_shift_ms(source_file, tracks_path, sources)?;
        for (tid, lang) in tracks_languages.iter().enumerate() {
            if let Some(lang) = lang {
                cmd.arg("--language").arg(format!("{tid}:{lang}"));
            }
            if shift != 0 {
                cmd.arg("--sync").arg(format!("{tid}:{shift}"));
            }
        }
        cmd.arg(tracks_path);
    }

    cmd.args(["--no-video", "--no-track-tags"]);
    cmd.args(sources.source_args());
    // mkvmerge flags a track default where the container has no such flag, as in MPEG-TS.
    for (tracks, spec) in [(&sources.source_audio, "a"), (&sources.source_subtitles, "s")] {
        let dispositions = if tracks.is_empty() { Vec::new() } else { probe_dispositions(source_file, spec) };
        for (index, id) in tracks {
            if let Some(stream) = dispositions.get(*index) {
                cmd.args(stream.disposition.to_mkvmerge_flags(*id as usize));
            }
        }
    }
    // A copied video keeps mkvmerge's timeline, and realign_copied_video moves these with it.
    if from_source && encoded {
        let shift = source_shift_ms + wrapped_ts_shift_ms(source_file, sources, output_path)?;
        if shift != 0 {
            cmd.arg("--sync").arg(format!("-1:{shift}"));
        }
    }
    if source_shift_ms != 0 && has_chapters(source_file)? {
        cmd.arg("--chapter-sync").arg(source_shift_ms.to_string());
    }
    cmd.arg(source_file);

    let out = crate::ext::mkvmerge_output(&mut cmd, crate::ext::whole_file_timeout(source_file, 3600), "mkvmerge")?;
    // Exit 1 also covers "track skipped: unsupported codec", which silently drops a track.
    if out.status.code() == Some(1) {
        let msg = String::from_utf8_lossy(&out.stdout);
        let warnings: Vec<&str> = msg.lines().filter(|l| l.contains("Warning")).collect();
        if !warnings.is_empty() {
            tracing::warn!("mkvmerge: {}", warnings.join(" | "));
        }
    }

    Ok(order)
}

pub struct Sources {
    pub extracted_audio: Vec<usize>,
    pub source_audio: Vec<(usize, u64)>,
    pub extracted_subtitles: Vec<usize>,
    pub source_subtitles: Vec<(usize, u64)>,
}

impl Sources {
    pub fn new(plan: &AudioPlan, subtitles: &crate::subtitle::SubtitlePlan) -> Self {
        Sources {
            extracted_audio: plan.extracted().iter().map(|t| t.audio_index).collect(),
            source_audio: plan.source_tracks(),
            extracted_subtitles: subtitles.extract.iter().map(|(i, _)| *i).collect(),
            source_subtitles: subtitles.from_source.clone(),
        }
    }

    fn source_args(&self) -> Vec<String> {
        let ids = |tracks: &[(usize, u64)]| tracks.iter().map(|(_, id)| id.to_string()).collect::<Vec<_>>().join(",");
        let pick = |tracks: &[(usize, u64)], flag: &str, none: &str| {
            if tracks.is_empty() { vec![none.to_string()] } else { vec![flag.to_string(), ids(tracks)] }
        };
        [pick(&self.source_audio, "--audio-tracks", "--no-audio"), pick(&self.source_subtitles, "--subtitle-tracks", "--no-subtitles")].concat()
    }
}

/// mkvmerge's file and track IDs in the source's order: video, then audio, then subtitles.
fn track_order(videos: &[u64], sources: &Sources, source_file: u32) -> Vec<(u32, u64)> {
    let by_index = |extracted: &[usize], first_id: usize, from_source: &[(usize, u64)]| {
        let mut tracks: Vec<(usize, (u32, u64))> = extracted.iter().enumerate()
            .map(|(n, &index)| (index, (1, (first_id + n) as u64)))
            .chain(from_source.iter().map(|&(index, id)| (index, (source_file, id))))
            .collect();
        tracks.sort_by_key(|(index, _)| *index);
        tracks.into_iter().map(|(_, track)| track)
    };
    videos.iter().map(|&id| (0, id))
        .chain(by_index(&sources.extracted_audio, 0, &sources.source_audio))
        .chain(by_index(&sources.extracted_subtitles, sources.extracted_audio.len(), &sources.source_subtitles))
        .collect()
}

/// ffmpeg starts an MPEG-TS extraction at the first packet it keeps, the video at the container start.
fn extraction_shift_ms(source: &Path, tracks_path: &Path, sources: &Sources) -> Result<i64> {
    #[derive(Deserialize)]
    struct Probe { #[serde(default)] streams: Vec<Stream>, #[serde(default)] format: Format }
    #[derive(Deserialize)]
    struct Stream { start_time: Option<String> }
    #[derive(Deserialize, Default)]
    struct Format { #[serde(default)] format_name: String, start_time: Option<String> }

    let spec = match (sources.extracted_audio.first(), sources.extracted_subtitles.first()) {
        (Some(i), _) => format!("a:{i}"),
        (None, Some(i)) => format!("s:{i}"),
        (None, None) => return Ok(0),
    };
    let probe = |path: &Path, spec: &str| -> Result<Probe> {
        crate::ext::ffprobe_json(
            &["-v", "error", "-select_streams", spec, "-show_entries", "stream=start_time:format=format_name,start_time", "-of", "json"],
            path,
        )
        .context("probe the start of the extracted tracks")
    };
    let source_probe = probe(source, &spec)?;
    if source_probe.format.format_name != "mpegts" {
        return Ok(0);
    }
    let extracted = probe(tracks_path, "0")?;
    let secs = |s: Option<&String>| s.and_then(|v| v.parse::<f64>().ok());
    let (Some(first), Some(container), Some(kept)) = (
        secs(source_probe.streams.first().and_then(|s| s.start_time.as_ref())),
        secs(source_probe.format.start_time.as_ref()),
        secs(extracted.streams.first().and_then(|s| s.start_time.as_ref())),
    ) else {
        return Ok(0);
    };
    Ok(((first - container - kept) * 1000.0).round() as i64)
}

/// mkvmerge keeps the absolute time of a wrapping MPEG-TS, and the tracks it takes land past the end.
fn wrapped_ts_shift_ms(source: &Path, sources: &Sources, scratch: &Path) -> Result<i64> {
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

    let taken = scratch.with_file_name("from_source.mkv");
    let mut cmd = crate::ext::mkvmerge();
    cmd.arg("-o").arg(&taken)
        .args(["--no-video", "--no-attachments", "--no-chapters", "--no-global-tags", "--no-track-tags"])
        .args(sources.source_args())
        .arg(source);
    let out = crate::ext::mkvmerge_output(&mut cmd, crate::ext::whole_file_timeout(source, 3600), "mkvmerge probe of the source tracks");
    if let Err(e) = out {
        let _ = std::fs::remove_file(&taken);
        return Err(e);
    }
    let packets: Result<Packets> = crate::ext::ffprobe_json(
        &["-v", "error", "-read_intervals", "%+#8", "-show_entries", "packet=pts_time", "-of", "json"],
        &taken,
    );
    let _ = std::fs::remove_file(&taken);
    let first = packets?.packets.iter().filter_map(|p| secs(p.pts_time.as_deref())).reduce(f64::min);
    Ok(absolute_time_shift_ms(first, start, duration))
}

fn absolute_time_shift_ms(first_packet: Option<f64>, start: f64, duration: f64) -> i64 {
    match first_packet {
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
    fn an_opus_bitrate_libopus_would_refuse_is_lowered_to_its_limit() {
        assert_eq!(opus_bitrate("320k".into(), "mono", 0), "256k");
        assert_eq!(opus_bitrate("600k".into(), "stereo", 0), "512k");
        assert_eq!(opus_bitrate("320k".into(), "stereo", 0), "320k");
        assert_eq!(opus_bitrate("1M".into(), "5.1", 0), "1M");
    }

    #[test]
    fn tracks_mkvmerge_takes_from_the_source_keep_their_place() {
        let sources = Sources {
            extracted_audio: vec![0, 2], source_audio: vec![(1, 5)],
            extracted_subtitles: vec![0, 2], source_subtitles: vec![(1, 7)],
        };
        assert_eq!(track_order(&[0], &sources, 2), [(0, 0), (1, 0), (2, 5), (1, 1), (1, 2), (2, 7), (1, 3)]);

        let only_source = Sources { extracted_audio: vec![], source_audio: vec![(0, 1)], extracted_subtitles: vec![], source_subtitles: vec![] };
        assert_eq!(track_order(&[3, 4], &only_source, 1), [(0, 3), (0, 4), (1, 1)]);
    }

    #[test]
    fn a_bcp_47_tag_is_kept_only_where_both_tools_agree_on_the_track() {
        let twin = |number, legacy, ietf| Twin { number, legacy, ietf };
        let by_place = |tracks: &[(Option<&'static str>, Option<&'static str>)]| -> Vec<Twin<'static>> {
            tracks.iter().map(|(legacy, ietf)| twin(None, *legacy, *ietf)).collect()
        };
        let untagged = |langs: &[Option<&'static str>]| langs.iter().map(|l| (None, *l)).collect::<Vec<_>>();
        let tags = |t: &[&str]| t.iter().map(|t| (!t.is_empty()).then(|| t.to_string())).collect::<Vec<_>>();

        let theirs = by_place(&[(Some("por"), Some("pt-BR")), (Some("por"), Some("pt-PT")), (Some("eng"), Some("en"))]);
        assert_eq!(matched_tags(&untagged(&[Some("por"), Some("por"), Some("eng")]), &theirs), tags(&["pt-BR", "pt-PT", "en"]));
        assert_eq!(matched_tags(&untagged(&[Some("ger"), Some("por"), Some("eng")]), &theirs), tags(&["", "pt-PT", "en"]));
        assert_eq!(matched_tags(&untagged(&[None, Some("por"), Some("eng")]), &theirs)[0], None);
        assert_eq!(matched_tags(&untagged(&[Some("por"), Some("por")]), &theirs), [None, None]);

        // Cantonese under the legacy code for all of Chinese, and MP4's `zho` for Matroska's `chi`.
        assert_eq!(matched_tags(&untagged(&[Some("chi")]), &by_place(&[(Some("chi"), Some("yue"))])), tags(&["yue"]));
        assert_eq!(matched_tags(&untagged(&[Some("zho")]), &by_place(&[(Some("chi"), Some("zh-Hant"))])), tags(&["zh-Hant"]));
    }

    #[test]
    fn a_ts_language_list_becomes_what_mkvmerge_read_for_that_pid() {
        let twin = |number, legacy, ietf| Twin { number: Some(number), legacy, ietf };
        let tags = |t: &[&str]| t.iter().map(|t| (!t.is_empty()).then(|| t.to_string())).collect::<Vec<_>>();

        let ts = [twin(0x101, Some("ger"), Some("de")), twin(0x102, Some("ger"), Some("de")), twin(0x103, Some("fre"), None)];
        let ours = [(Some("0x101"), Some("ger,eng")), (Some("0x102"), Some("ger")), (Some("0x103"), Some("fre,eng"))];
        assert_eq!(matched_tags(&ours, &ts), tags(&["de", "de", "fre"]));

        let ours = [(Some("0x100"), Some("eng")), (Some("0x101"), Some("ger,eng")), (Some("0x103"), Some("fre,eng"))];
        assert_eq!(matched_tags(&ours, &ts), tags(&["", "de", "fre"]));

        let unknown_first = [twin(0x101, None, None), twin(0x102, Some("fre"), None)];
        let ours = [(Some("0x101"), Some("zzz,eng")), (Some("0x102"), Some("fre,eng"))];
        assert_eq!(matched_tags(&ours, &unknown_first), tags(&["", "fre"]));
        let ours = [(Some("0x100"), Some("eng")), (Some("0x101"), Some("zzz,eng")), (Some("0x109"), Some("zzz,eng"))];
        assert_eq!(matched_tags(&ours, &unknown_first), tags(&["", "", ""]));
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
        assert!(is_lossless("pcm_s24le", None, &set));
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
            sample_fmt: fmt.into(), initial_padding: 0, stream_id: None, language: None, title: None,
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
