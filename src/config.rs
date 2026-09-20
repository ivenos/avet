use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub encoder: Option<Encoder>,
    #[serde(default)]
    pub encoder_params: HashMap<String, toml::Value>,
    #[serde(default)]
    pub avet: AvetConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub subtitles: SubtitleConfig,
    #[serde(default)]
    pub scene_detection: SceneDetectionConfig,
    /// Per-chunk CVVDP JOD target instead of a fixed CRF. None = fixed CRF.
    pub target_quality: Option<TargetQualityConfig>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Encoder {
    SvtAv1,
    SvtAv1Hdr,
}

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VideoMode {
    #[default]
    Encode,
    Copy,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct AvetConfig {
    #[serde(default)]
    pub video: VideoMode,
    #[serde(default)]
    pub dv: bool,
    #[serde(default)]
    pub crop: bool,
    #[serde(default)]
    pub keyint: bool,
    pub scale: Option<u32>,
    pub bit_depth: Option<u8>,
    #[serde(default)]
    pub keep_temp: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct SubtitleConfig {
    #[serde(default)]
    pub mode: SubtitleMode,
    #[serde(default)]
    pub language_whitelist: Vec<String>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleMode {
    #[default]
    Copy,
    Strip,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct AudioConfig {
    #[serde(default)]
    pub mode: AudioMode,
    pub codec: Option<String>,
    pub bitrate: Option<Bitrate>,
    #[serde(default)]
    pub options: HashMap<String, toml::Value>,
    #[serde(default)]
    pub language_whitelist: Vec<String>,
    /// Override for lossless sources; unset fields inherit from [audio].
    pub lossless: Option<AudioProfile>,
    #[serde(default)]
    pub codec_rules: HashMap<String, AudioProfile>,
}

/// Override whose unset fields inherit from [audio].
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct AudioProfile {
    pub mode: Option<AudioMode>,
    pub codec: Option<String>,
    pub bitrate: Option<Bitrate>,
    #[serde(default)]
    pub options: HashMap<String, toml::Value>,
}

/// A single bitrate, or a per-layout table keyed by layout name (plus `default`).
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Bitrate {
    Single(String),
    PerLayout(HashMap<String, String>),
}

impl Bitrate {
    pub fn resolve(&self, channels: Option<u32>) -> Option<&str> {
        match self {
            Bitrate::Single(s) => Some(s.as_str()),
            Bitrate::PerLayout(map) => channels
                .and_then(|c| map.get(layout_name(c)))
                .or_else(|| map.get("default"))
                .map(String::as_str),
        }
    }

    /// Keyed by the layout that is written, which an Opus upmix widens past the source.
    pub fn resolve_layout(&self, layout: &str) -> Option<&str> {
        match self {
            Bitrate::Single(s) => Some(s.as_str()),
            Bitrate::PerLayout(map) => map
                .get(layout)
                .or_else(|| map.get("default"))
                .map(String::as_str),
        }
    }
}

pub struct ResolvedAudio<'a> {
    pub mode: AudioMode,
    pub codec: Option<&'a str>,
    pub bitrate: Option<&'a Bitrate>,
    pub options: &'a HashMap<String, toml::Value>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum AudioMode {
    #[default]
    Copy,
    Encode,
}

/// Layout name for a channel count (per-layout bitrate key).
pub fn layout_name(channels: u32) -> &'static str {
    match channels {
        0 | 1 => "mono",
        2 => "stereo",
        3 => "3.0",
        4 => "quad",
        5 => "5.0",
        6 => "5.1",
        7 => "6.1",
        _ => "7.1",
    }
}

/// ISO 639-2 spells ~20 languages twice: `ger` in Matroska, `deu` in a config.
fn iso639_alias(code: &str) -> Option<&'static str> {
    Some(match code {
        "alb" => "sqi", "sqi" => "alb",
        "arm" => "hye", "hye" => "arm",
        "baq" => "eus", "eus" => "baq",
        "bur" => "mya", "mya" => "bur",
        "chi" => "zho", "zho" => "chi",
        "cze" => "ces", "ces" => "cze",
        "dut" => "nld", "nld" => "dut",
        "fre" => "fra", "fra" => "fre",
        "geo" => "kat", "kat" => "geo",
        "ger" => "deu", "deu" => "ger",
        "gre" => "ell", "ell" => "gre",
        "ice" => "isl", "isl" => "ice",
        "mac" => "mkd", "mkd" => "mac",
        "mao" => "mri", "mri" => "mao",
        "may" => "msa", "msa" => "may",
        "per" => "fas", "fas" => "per",
        "rum" => "ron", "ron" => "rum",
        "slo" => "slk", "slk" => "slo",
        "tib" => "bod", "bod" => "tib",
        "wel" => "cym", "cym" => "wel",
        _ => return None,
    })
}

/// Case-insensitive, alias-aware, subtags ignored. Same code set only: `de-DE` is not `ger`.
pub fn language_matches(entry: &str, tag: &str) -> bool {
    let base_of = |s: &str| {
        s.trim()
            .to_ascii_lowercase()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let entry = base_of(entry);
    let tag = base_of(tag);

    !entry.is_empty()
        && (entry == tag || iso639_alias(&entry).is_some_and(|a| a == tag))
}

/// Empty whitelist or untagged track keeps everything; MP4 spells untagged `und`.
pub fn language_selected(whitelist: &[String], tag: Option<&str>) -> bool {
    if whitelist.is_empty() {
        return true;
    }
    let tagged = tag
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("und"));

    match tagged {
        None => true,
        Some(lang) => whitelist.iter().any(|w| language_matches(w, lang)),
    }
}

/// True if the output codec is lossless (bitrate then irrelevant). ffmpeg's lossless
/// audio encoders, which is fewer than the lossless formats it decodes.
pub fn output_is_lossless(codec: &str) -> bool {
    matches!(codec, "flac" | "alac" | "wavpack" | "tta" | "truehd" | "mlp" | "s302m")
        || codec.starts_with("pcm_")
}

/// Stringify a TOML value for the ffmpeg/encoder CLI (booleans as 1/0).
pub(crate) fn toml_value_to_arg(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s)  => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f)   => f.to_string(),
        toml::Value::Boolean(b) => if *b { "1".into() } else { "0".into() },
        other                   => other.to_string(),
    }
}

impl AudioConfig {
    /// Resolve a track: codec_rules, then lossless override, then [audio].
    pub fn resolve(&self, codec_name: &str, is_lossless: bool) -> ResolvedAudio<'_> {
        if let Some(rule) = self.codec_rules.get(codec_name) {
            return self.overlay(rule);
        }
        if is_lossless && let Some(p) = &self.lossless {
            return self.overlay(p);
        }
        ResolvedAudio {
            mode: self.mode,
            codec: self.codec.as_deref(),
            bitrate: self.bitrate.as_ref(),
            options: &self.options,
        }
    }

    /// Apply an override over [audio], inheriting unset fields.
    fn overlay<'a>(&'a self, ov: &'a AudioProfile) -> ResolvedAudio<'a> {
        ResolvedAudio {
            mode: ov.mode.unwrap_or(self.mode),
            codec: ov.codec.as_deref().or(self.codec.as_deref()),
            bitrate: ov.bitrate.as_ref().or(self.bitrate.as_ref()),
            options: if ov.options.is_empty() { &self.options } else { &ov.options },
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SceneDetectionSpeedConfig {
    #[default]
    Standard,
    Fast,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct SceneDetectionConfig {
    /// Minimum number of frames between scene cuts.
    pub min_scene_len: usize,
    /// Max chunk length in seconds; 0 disables it, `extra_split` > 0 overrides it.
    pub extra_split_sec: u32,
    /// Maximum scene length in frames. Overrides `extra_split_sec` when > 0. Set to 0 to disable.
    pub extra_split: u32,
    /// Scene detection algorithm speed.
    pub speed: SceneDetectionSpeedConfig,
    /// Downscale height for scene detection only (e.g. 720). None = no extra downscale.
    pub downscale_height: Option<u32>,
}

impl Default for SceneDetectionConfig {
    fn default() -> Self {
        Self {
            min_scene_len: 24,
            extra_split_sec: 10,
            extra_split: 0,
            speed: SceneDetectionSpeedConfig::default(),
            downscale_height: None,
        }
    }
}

/// One encoder process and one resume entry per frame below this.
const MIN_EXTRA_SPLIT: u32 = 24;

impl SceneDetectionConfig {
    /// Only used after indexing and detection, so a bad value would cost minutes first.
    fn validate(&self) -> Result<()> {
        if self.min_scene_len == 0 {
            bail!("scene_detection.min_scene_len must be >= 1");
        }
        if self.extra_split > 0 && self.extra_split < MIN_EXTRA_SPLIT {
            bail!(
                "scene_detection.extra_split must be >= {MIN_EXTRA_SPLIT} frames (got {}); use 0 to disable",
                self.extra_split
            );
        }
        if let Some(h) = self.downscale_height {
            // A zero edge reads as "keep the input size" to ffmpeg, same as avet.scale.
            if h < 64 {
                bail!(
                    "scene_detection.downscale_height must be at least 64 (got {h}); \
                     remove it to disable the extra downscale"
                );
            }
        }
        Ok(())
    }

    /// Returns the effective max chunk size in frames, or None if extra splitting is disabled.
    pub fn effective_extra_split_frames(&self, fps: f64) -> Option<usize> {
        if self.extra_split > 0 {
            Some(self.extra_split as usize)
        } else if self.extra_split_sec > 0 {
            // A slideshow's frame rate turns the same number of seconds into single frames.
            let frames = (self.extra_split_sec as f64 * fps).round() as usize;
            if frames > 0 { Some(frames.max(MIN_EXTRA_SPLIT as usize)) } else { None }
        } else {
            None
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct TargetQualityConfig {
    /// CVVDP JOD score to hold as a hard minimum per chunk. 0 is rejected by validate().
    pub jod: f64,
    /// CRF search bounds.
    pub min_crf: u32,
    pub max_crf: u32,
    /// Probe budget per chunk. The search stops early once it converges.
    pub min_probes: u32,
    pub max_probes: u32,
    /// Stop early when a probe lands at most this far above the floor.
    pub tolerance: f64,
    pub probe_preset: u32,
    /// Encoded size ceiling as a percent of the source over the chunk duration.
    pub max_encoded_percent: f64,
    pub max_cambi: Option<f64>,
    pub max_cambi_diff: Option<f64>,
}

impl Default for TargetQualityConfig {
    fn default() -> Self {
        Self {
            jod: 0.0,
            min_crf: 1,
            max_crf: 70,
            min_probes: 2,
            max_probes: 7,
            tolerance: 0.05,
            probe_preset: 13,
            max_encoded_percent: 90.0,
            max_cambi: None,
            max_cambi_diff: None,
        }
    }
}

impl Config {
    /// Parse and validate without a file; same path as `from_file` from the parse on.
    #[cfg(test)]
    pub fn from_str_for_test(raw: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(raw).context("parse encode.toml")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read encode.toml: {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("parse encode.toml: {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.avet.video != VideoMode::Copy && self.encoder.is_none() {
            bail!("encoder is required unless avet.video = \"copy\"");
        }
        if let Some(d) = self.avet.bit_depth
            && d != 8 && d != 10
        {
            bail!("avet.bit_depth must be 8 or 10 (got {d})");
        }
        // A zero edge reads as "keep the input size" to ffmpeg.
        if let Some(h) = self.avet.scale
            && h < 64
        {
            bail!("avet.scale must be at least 64 (got {h}); remove it to disable scaling");
        }
        if let Some(tq) = &self.target_quality {
            if self.avet.video == VideoMode::Copy {
                bail!("target_quality requires avet.video = \"encode\"");
            }
            // FFVship scores the encode against the source at source resolution, so a
            // downscale reads as loss: 1080p to 540p alone measures 0.8 JOD below the
            // untouched encode, and every chunk burns its whole probe budget for nothing.
            if self.avet.scale.is_some() {
                bail!(
                    "target_quality cannot be combined with avet.scale: the quality score \
                     compares against the source at its own resolution, so the downscale \
                     itself counts as a loss and no CRF reaches the floor. Remove one of them."
                );
            }
            if tq.jod == 0.0 {
                bail!("target_quality.jod is required: the CVVDP JOD floor to hold, in (0, 10)");
            }
            if !(tq.jod > 0.0 && tq.jod < 10.0) {
                // 10 is the top of the scale, reachable only by an identical image.
                bail!("target_quality.jod must be in (0, 10) (got {})", tq.jod);
            }
            if tq.min_crf < 1 {
                bail!("target_quality.min_crf must be >= 1 (got {})", tq.min_crf);
            }
            if tq.min_crf >= tq.max_crf {
                bail!("target_quality.min_crf must be < max_crf ({} >= {})", tq.min_crf, tq.max_crf);
            }
            if tq.max_crf > 70 {
                bail!("target_quality.max_crf must be <= 70 (got {})", tq.max_crf);
            }
            if tq.min_probes < 2 {
                bail!("target_quality.min_probes must be >= 2 (got {})", tq.min_probes);
            }
            if tq.max_probes < tq.min_probes {
                bail!("target_quality.max_probes must be >= min_probes ({} < {})", tq.max_probes, tq.min_probes);
            }
            if tq.probe_preset > 13 {
                bail!("target_quality.probe_preset must be 0..=13 (got {})", tq.probe_preset);
            }
            // TOML accepts `nan`, and every comparison against it is false.
            if !tq.tolerance.is_finite() || tq.tolerance < 0.0 {
                bail!("target_quality.tolerance must be a finite value >= 0 (got {})", tq.tolerance);
            }
            if !tq.max_encoded_percent.is_finite() || tq.max_encoded_percent <= 0.0 {
                bail!(
                    "target_quality.max_encoded_percent must be a finite value > 0 (got {})",
                    tq.max_encoded_percent
                );
            }
            for (key, limit) in [("max_cambi", tq.max_cambi), ("max_cambi_diff", tq.max_cambi_diff)] {
                if let Some(c) = limit
                    && (!c.is_finite() || c < 0.0)
                {
                    bail!("target_quality.{key} must be a finite value >= 0 (got {c})");
                }
            }
        }
        self.scene_detection.validate()?;
        validate_audio("audio", self.audio.mode, self.audio.codec.as_deref(), self.audio.bitrate.as_ref())?;
        if let Some(p) = &self.audio.lossless {
            let r = self.audio.overlay(p);
            validate_audio("audio.lossless", r.mode, r.codec, r.bitrate)?;
        }
        for (source_codec, rule) in &self.audio.codec_rules {
            let r = self.audio.overlay(rule);
            validate_audio(&format!("audio.codec_rules.{source_codec}"), r.mode, r.codec, r.bitrate)?;
        }
        Ok(())
    }

    pub fn encoder_args(&self) -> Vec<String> {
        let mut entries: Vec<_> = self.encoder_params.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let mut args = Vec::with_capacity(entries.len() * 2);
        for (k, v) in entries {
            args.push(format!("--{k}"));
            args.push(toml_value_to_arg(v));
        }
        args
    }
}

/// Encode needs a codec; lossy codecs also need a bitrate.
fn validate_audio(ctx: &str, mode: AudioMode, codec: Option<&str>, bitrate: Option<&Bitrate>) -> Result<()> {
    validate_bitrate_keys(ctx, bitrate)?;
    validate_bitrate_values(ctx, bitrate)?;
    if mode != AudioMode::Encode {
        return Ok(());
    }
    let Some(codec) = codec else {
        bail!("{ctx}: codec required when mode = encode");
    };
    if !output_is_lossless(codec) && bitrate.is_none() {
        bail!("{ctx}: bitrate required when mode = encode ({codec} is lossy)");
    }
    Ok(())
}

/// ffmpeg reads a bare `192` as 192 bit/s and encodes it without complaint, which is only
/// visible in the finished file.
fn validate_bitrate_values(ctx: &str, bitrate: Option<&Bitrate>) -> Result<()> {
    let values: Vec<&String> = match bitrate {
        None => return Ok(()),
        Some(Bitrate::Single(s)) => vec![s],
        Some(Bitrate::PerLayout(map)) => map.values().collect(),
    };
    for value in values {
        let bits = parse_bitrate(value)
            .with_context(|| format!("{ctx}: bitrate \"{value}\" is not a number with an optional k or M suffix"))?;
        if bits < 1000 {
            bail!("{ctx}: bitrate \"{value}\" is {bits} bit/s; write it as \"{value}k\" for kbit/s");
        }
    }
    Ok(())
}

/// The forms ffmpeg's `-b:a` takes: a plain count of bits, or one with a k/M suffix.
fn parse_bitrate(value: &str) -> Result<u64> {
    let text = value.trim();
    let (digits, factor) = match text.as_bytes().last() {
        Some(b'k' | b'K') => (&text[..text.len() - 1], 1_000),
        Some(b'm' | b'M') => (&text[..text.len() - 1], 1_000_000),
        _ => (text, 1),
    };
    let number: f64 = digits.parse().map_err(|_| anyhow::anyhow!("not a number"))?;
    if !number.is_finite() || number <= 0.0 {
        bail!("not a positive number");
    }
    Ok((number * factor as f64) as u64)
}

/// A plain table, so `deny_unknown_fields` cannot reach its keys.
fn validate_bitrate_keys(ctx: &str, bitrate: Option<&Bitrate>) -> Result<()> {
    let Some(Bitrate::PerLayout(map)) = bitrate else {
        return Ok(());
    };
    for key in map.keys() {
        let known = key == "default" || (0..=8).any(|c| layout_name(c) == key);
        if !known {
            bail!(
                "{ctx}: unknown bitrate layout \"{key}\"; \
                 expected one of mono, stereo, 3.0, quad, 5.0, 5.1, 6.1, 7.1, default"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_bit_depth(d: Option<u8>) -> Config {
        Config {
            encoder: Some(Encoder::SvtAv1),
            avet: AvetConfig { bit_depth: d, ..Default::default() },
            ..Default::default()
        }
    }

    #[test]
    fn bit_depth_8_and_10_are_valid() {
        cfg_with_bit_depth(Some(8)).validate().unwrap();
        cfg_with_bit_depth(Some(10)).validate().unwrap();
        cfg_with_bit_depth(None).validate().unwrap();
    }

    #[test]
    fn bit_depth_other_values_rejected() {
        for d in [0u8, 9, 12, 16] {
            let err = cfg_with_bit_depth(Some(d)).validate().unwrap_err();
            assert!(
                err.to_string().contains("bit_depth"),
                "expected bit_depth error for {d}, got: {err}"
            );
        }
    }

    fn audio(toml_str: &str) -> AudioConfig {
        toml::from_str(toml_str).expect("parse audio config")
    }

    #[test]
    fn video_mode_defaults_to_encode() {
        let c: Config = toml::from_str(r#"encoder = "svt-av1""#).unwrap();
        assert_eq!(c.avet.video, VideoMode::Encode);
        let c: Config = toml::from_str("encoder = \"svt-av1\"\n[avet]\nvideo = \"copy\"").unwrap();
        assert_eq!(c.avet.video, VideoMode::Copy);
    }

    #[test]
    fn bitrate_parses_single_and_per_layout() {
        let a = audio(r#"bitrate = "192k""#);
        assert!(matches!(a.bitrate, Some(Bitrate::Single(ref s)) if s == "192k"));

        let a = audio(r#"bitrate = { stereo = "192k", "5.1" = "320k" }"#);
        assert!(matches!(a.bitrate, Some(Bitrate::PerLayout(_))));
    }

    #[test]
    fn layout_name_maps_channel_counts() {
        assert_eq!(layout_name(1), "mono");
        assert_eq!(layout_name(2), "stereo");
        assert_eq!(layout_name(6), "5.1");
        assert_eq!(layout_name(8), "7.1");
        assert_eq!(layout_name(16), "7.1");
    }

    #[test]
    fn bitrate_follows_the_layout_that_is_written_not_the_source() {
        let b = Bitrate::PerLayout(HashMap::from([
            ("3.0".into(), "128k".into()),
            ("5.1".into(), "320k".into()),
        ]));
        assert_eq!(b.resolve(Some(3)), Some("128k"));
        assert_eq!(b.resolve_layout("5.1"), Some("320k"));
        assert_eq!(b.resolve_layout("6.1"), None);
    }

    #[test]
    fn bitrate_resolve_by_channels_with_default() {
        let b = Bitrate::PerLayout(HashMap::from([
            ("stereo".into(), "192k".into()),
            ("5.1".into(), "320k".into()),
            ("default".into(), "256k".into()),
        ]));
        assert_eq!(b.resolve(Some(2)), Some("192k"));
        assert_eq!(b.resolve(Some(6)), Some("320k"));
        assert_eq!(b.resolve(Some(8)), Some("256k")); // falls back to default
        assert_eq!(b.resolve(None), Some("256k"));

        let single = Bitrate::Single("128k".into());
        assert_eq!(single.resolve(Some(6)), Some("128k"));
    }

    #[test]
    fn output_lossless_classification() {
        assert!(output_is_lossless("truehd"));
        assert!(output_is_lossless("flac"));
        assert!(output_is_lossless("pcm_s24le"));
        assert!(!output_is_lossless("libopus"));
        assert!(!output_is_lossless("aac"));
    }

    #[test]
    fn flac_encode_needs_no_bitrate_but_opus_does() {
        validate_audio("audio", AudioMode::Encode, Some("flac"), None).unwrap();
        assert!(validate_audio("audio", AudioMode::Encode, Some("libopus"), None).is_err());
        assert!(validate_audio("audio", AudioMode::Encode, None, None).is_err());
    }

    #[test]
    fn resolve_precedence_and_inheritance() {
        let cfg = audio(
            r#"
            mode    = "encode"
            codec   = "libopus"
            bitrate = "192k"
            [lossless]
            codec   = "flac"
            [codec_rules]
            opus = { mode = "copy" }
            "#,
        );
        assert_eq!(cfg.resolve("opus", false).mode, AudioMode::Copy);
        // lossless override keeps inherited mode + bitrate
        let r = cfg.resolve("truehd", true);
        assert_eq!(r.mode, AudioMode::Encode);
        assert_eq!(r.codec, Some("flac"));
        let r = cfg.resolve("eac3", false);
        assert_eq!(r.codec, Some("libopus"));
        assert!(matches!(r.bitrate, Some(Bitrate::Single(s)) if s == "192k"));
    }

    #[test]
    fn target_quality_defaults_and_valid() {
        let c: Config = toml::from_str("encoder = \"svt-av1\"\n[target_quality]\njod = 9.5").unwrap();
        c.validate().unwrap();
        let tq = c.target_quality.unwrap();
        assert_eq!((tq.min_crf, tq.max_crf, tq.min_probes, tq.max_probes, tq.probe_preset), (1, 70, 2, 7, 13));
        assert_eq!((tq.tolerance, tq.max_encoded_percent), (0.05, 90.0));
        assert_eq!((tq.max_cambi, tq.max_cambi_diff), (None, None));

        let c = Config::from_str_for_test(
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmax_cambi = 5\nmax_cambi_diff = 1.5",
        )
        .unwrap();
        let tq = c.target_quality.unwrap();
        assert_eq!((tq.max_cambi, tq.max_cambi_diff), (Some(5.0), Some(1.5)));
    }

    #[test]
    fn target_quality_rejects_bad_values() {
        let bad = [
            "encoder = \"svt-av1\"\n[target_quality]\njod = 0",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 11",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmin_crf = 40\nmax_crf = 30",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmin_crf = 0",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmax_crf = 71",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmin_probes = 1",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmin_probes = 5\nmax_probes = 3",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nprobe_preset = 14",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmax_encoded_percent = 0",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmax_cambi = -1",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\nmax_cambi_diff = -0.5",
            "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\ntolerance = -0.1",
        ];
        for t in bad {
            let c: Config = toml::from_str(t).unwrap();
            assert!(c.validate().is_err(), "should reject:\n{t}");
        }
    }

    #[test]
    fn a_misspelled_enum_value_is_refused_instead_of_falling_back_to_the_default() {
        for t in [
            "encoder = \"x264\"",
            "encoder = \"svt-av1\"\n[avet]\nvideo = \"kopie\"",
            "encoder = \"svt-av1\"\n[subtitles]\nmode = \"remove\"",
            "encoder = \"svt-av1\"\n[audio]\nmode = \"transcode\"",
            "encoder = \"svt-av1\"\n[scene_detection]\nspeed = \"slow\"",
        ] {
            assert!(Config::from_str_for_test(t).is_err(), "should reject:\n{t}");
        }
    }

    #[test]
    fn a_lossless_override_onto_a_lossy_codec_still_needs_its_bitrate() {
        let profile = |body: &str| format!("encoder = \"svt-av1\"\n[audio.lossless]\n{body}");
        let err = Config::from_str_for_test(&profile("mode = \"encode\"\ncodec = \"libopus\""))
            .unwrap_err();
        assert!(format!("{err:#}").contains("audio.lossless"), "got: {err:#}");

        Config::from_str_for_test(&profile("mode = \"encode\"\ncodec = \"flac\"")).unwrap();
        Config::from_str_for_test(&profile("mode = \"encode\"\ncodec = \"libopus\"\nbitrate = \"256k\"")).unwrap();
    }

    #[test]
    fn target_quality_requires_encode_video() {
        let c: Config = toml::from_str(
            "encoder = \"svt-av1\"\n[avet]\nvideo = \"copy\"\n[target_quality]\njod = 9.5",
        )
        .unwrap();
        assert!(c.validate().is_err());
    }

    #[test]
    fn target_quality_and_scale_are_refused_together() {
        // The score compares against the source, so the downscale reads as lost quality.
        let err = Config::from_str_for_test(
            "encoder = \"svt-av1\"\n[avet]\nscale = 1080\n[target_quality]\njod = 9.5",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("avet.scale"), "got: {err}");

        Config::from_str_for_test("encoder = \"svt-av1\"\n[target_quality]\njod = 9.5").unwrap();
        Config::from_str_for_test("encoder = \"svt-av1\"\n[avet]\nscale = 1080").unwrap();
    }

    #[test]
    fn language_whitelist_matches_both_iso_639_2_spellings() {
        // Matroska tends to carry the bibliographic code, configs the terminological one.
        assert!(language_matches("deu", "ger"));
        assert!(language_matches("ger", "deu"));
        assert!(language_matches("fra", "fre"));
        assert!(language_matches("zho", "chi"));
        assert!(language_matches("deu", "deu"));
        assert!(!language_matches("deu", "eng"));
        // Arabic and Armenian share a two-letter prefix; a prefix match would confuse them.
        assert!(!language_matches("ara", "arm"));
    }

    #[test]
    fn language_whitelist_ignores_case_and_region_suffix() {
        assert!(language_matches("POR", "por-BR"));
        assert!(language_matches("pt", "pt-BR"));
        assert!(language_matches("por", "por"));
        assert!(language_matches(" eng ", "eng"));
        assert!(!language_matches("por", "spa"));
        assert!(language_matches("por-BR", "por"));
        assert!(language_matches("por_BR", "por-PT"));
        assert!(language_matches("deu-DE", "ger"));
        // Same code set on both sides, subtag or not.
        assert!(!language_matches("por", "pt-BR"));
        assert!(!language_matches("de-DE", "ger"));
        assert!(!language_matches("", "eng"));
    }

    #[test]
    fn scene_detection_values_are_checked_before_the_encode_starts() {
        let bad = |toml: &str| {
            Config::from_str_for_test(toml)
                .unwrap_err()
                .to_string()
        };
        let sd = |body: &str| format!("encoder = \"svt-av1\"\n[scene_detection]\n{body}");
        assert!(bad(&sd("min_scene_len = 0")).contains("min_scene_len"));
        assert!(bad(&sd("extra_split = 1")).contains("extra_split"));
        assert!(bad(&sd("downscale_height = 0")).contains("downscale_height"));

        Config::from_str_for_test(&sd("extra_split = 240\ndownscale_height = 720")).unwrap();
    }

    #[test]
    fn a_low_frame_rate_does_not_turn_extra_split_sec_into_single_frame_chunks() {
        let cfg = SceneDetectionConfig { extra_split_sec: 1, ..Default::default() };
        assert_eq!(cfg.effective_extra_split_frames(1.0), Some(24));
        assert_eq!(cfg.effective_extra_split_frames(24.0), Some(24));
        assert_eq!(cfg.effective_extra_split_frames(60.0), Some(60));

        let off = SceneDetectionConfig { extra_split_sec: 0, ..Default::default() };
        assert_eq!(off.effective_extra_split_frames(24.0), None);
    }

    #[test]
    fn non_finite_target_quality_values_are_rejected() {
        let bad = |toml: &str| Config::from_str_for_test(toml).unwrap_err().to_string();
        let base = "encoder = \"svt-av1\"\n[target_quality]\njod = 9.5\n";
        assert!(bad(&format!("{base}max_encoded_percent = nan")).contains("max_encoded_percent"));
        assert!(bad(&format!("{base}max_encoded_percent = inf")).contains("max_encoded_percent"));
        assert!(bad(&format!("{base}tolerance = nan")).contains("tolerance"));
        assert!(bad(&format!("{base}max_cambi = nan")).contains("max_cambi"));
        assert!(bad(&format!("{base}max_cambi_diff = inf")).contains("max_cambi_diff"));
    }

    #[test]
    fn unknown_bitrate_layout_keys_are_rejected() {
        let toml ="encoder = \"svt-av1\"\n[audio]\nmode = \"encode\"\ncodec = \"libopus\"\n\
                    bitrate = { stereo = \"192k\", \"5,1\" = \"320k\" }";
        let err = Config::from_str_for_test(toml).unwrap_err().to_string();
        assert!(err.contains("5,1"), "got: {err}");

        Config::from_str_for_test(
            "encoder = \"svt-av1\"\n[audio]\nmode = \"encode\"\ncodec = \"libopus\"\n\
             bitrate = { stereo = \"192k\", \"5.1\" = \"320k\", default = \"128k\" }",
        )
        .unwrap();
    }

    #[test]
    fn a_bitrate_missing_its_k_is_rejected_before_anything_is_encoded() {
        let profile = |bitrate: &str| format!(
            "encoder = \"svt-av1\"\n[audio]\nmode = \"encode\"\ncodec = \"libopus\"\nbitrate = {bitrate}"
        );

        for good in ["\"192k\"", "\"192000\"", "\"1M\"", "{ stereo = \"192k\", default = \"320k\" }"] {
            Config::from_str_for_test(&profile(good)).unwrap_or_else(|e| panic!("{good}: {e:#}"));
        }

        // ffmpeg encodes this at 192 bit/s and reports success.
        let err = Config::from_str_for_test(&profile("\"192\"")).unwrap_err().to_string();
        assert!(err.contains("192 bit/s"), "got: {err}");

        let err = Config::from_str_for_test(&profile("\"loud\"")).unwrap_err().to_string();
        assert!(err.contains("loud"), "got: {err}");

        let err = Config::from_str_for_test(&profile("{ stereo = \"128\" }")).unwrap_err().to_string();
        assert!(err.contains("128 bit/s"), "got: {err}");
    }

    #[test]
    fn und_counts_as_untagged_not_as_a_language() {
        let wl = vec!["eng".to_string()];
        assert!(language_selected(&wl, None));
        assert!(language_selected(&wl, Some("und")));
        assert!(language_selected(&wl, Some("UND")));
        assert!(language_selected(&wl, Some("")));
        assert!(language_selected(&wl, Some("eng")));
        assert!(!language_selected(&wl, Some("ger")));
        assert!(language_selected(&[], Some("ger")));
    }

    #[test]
    fn scale_below_the_minimum_is_rejected() {
        for bad in [0u32, 1, 63] {
            let c: Config = toml::from_str(&format!(
                "encoder = \"svt-av1\"\n[avet]\nscale = {bad}\n"
            ))
            .unwrap();
            assert!(c.validate().is_err(), "scale = {bad} should be rejected");
        }
        let c: Config = toml::from_str("encoder = \"svt-av1\"\n[avet]\nscale = 720\n").unwrap();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_ignored() {
        // A misspelled section used to parse as "feature not configured".
        assert!(toml::from_str::<Config>("encoder = \"svt-av1\"\n[target_qualtiy]\njod = 9.5\n").is_err());
        assert!(toml::from_str::<Config>("encoder = \"svt-av1\"\n[avet]\nbitdepth = 10\n").is_err());
        assert!(toml::from_str::<Config>("encoder = \"svt-av1\"\n[target_quality]\nmax_probe = 2\n").is_err());
    }

    #[test]
    fn dv_stands_alone() {
        let c = Config::from_str_for_test("encoder = \"svt-av1\"\n[avet]\ndv = true\n").unwrap();
        assert!(c.avet.dv);
        assert!(Config::from_str_for_test("encoder = \"svt-av1\"\n[avet]\nhdr = true\n").is_err());
    }

    #[test]
    fn missing_jod_is_named_as_missing() {
        let c: Config = toml::from_str("encoder = \"svt-av1\"\n[target_quality]\nmin_crf = 10\n").unwrap();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("jod is required"), "got: {err}");
    }
}
