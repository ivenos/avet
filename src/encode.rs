use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::config::{Config, Encoder};
use crate::ffms2::{Crop, OpenOpts, VideoSource};
use crate::ext::external_bin;
use crate::resume::SceneEntry;

/// Per-call overrides; CRF is fractional, the SVT-AV1 encoders accept 0.25 steps.
#[derive(Default, Clone, Copy)]
pub struct EncodeOverrides {
    pub crf: Option<f64>,
    pub preset: Option<u32>,
}

#[derive(Clone, Default)]
pub struct EncodeOptions {
    /// SVT-AV1 HDR args (color-primaries, transfer, etc.)
    pub hdr_args: Vec<String>,
    /// Auto-keyint; skipped if user set "keyint" in encoder_params.
    pub keyint: Option<u32>,
    /// Output scale target; applied by ffmpeg after the crop (crop before scale).
    pub scale: Option<(u32, u32)>,
    /// Crop in source space, applied in the Y4M pipe before scaling.
    pub crop: Option<Crop>,
    /// FPS from ffprobe; FFMS2 reports 0/0 for some exotic containers (e.g. DV) which breaks IVF timestamps.
    pub fps_num: u32,
    pub fps_den: u32,
    /// Forced encoder input bit depth (8 or 10); None = pass source through.
    pub target_bit_depth: Option<u8>,
}

pub fn encode_chunk(
    source_file: &Path,
    index_file: &Path,
    scene: &SceneEntry,
    output_path: &Path,
    config: &Config,
    opts: &EncodeOptions,
    overrides: EncodeOverrides,
) -> Result<u64> {
    let encoder = config.encoder.context("encoder is required when video is encoded")?;
    let encoder_name = encoder_binary(encoder);
    let encoder_bin = external_bin(encoder_name);
    let mut encoder_args = build_encoder_args(config, output_path, opts)?;
    if let Some(crf) = overrides.crf {
        set_arg(&mut encoder_args, "--crf", crf.to_string());
    }
    if let Some(preset) = overrides.preset {
        set_arg(&mut encoder_args, "--preset", preset.to_string());
    }

    let mut vs = VideoSource::open(
        source_file,
        index_file,
        OpenOpts { target_bit_depth: opts.target_bit_depth },
    )
    .context("open FFMS2 VideoSource")?;
    // Override FFMS2 fps with ffprobe value (FFMS2 returns 0/0 for some containers, corrupting IVF timestamps).
    vs.info.fps_num = opts.fps_num;
    vs.info.fps_den = opts.fps_den;

    match opts.scale {
        Some(scale) => encode_scaled(
            &encoder_bin, encoder_name, &encoder_args,
            &mut vs, scene, opts.crop, scale,
        )?,
        None => encode_direct(
            &encoder_bin, encoder_name, &encoder_args,
            &mut vs, scene, opts.crop,
        )?,
    }

    chunk_size(output_path)
}

/// FFMS2 Y4M piped straight into the encoder.
fn encode_direct(
    encoder_bin: &OsStr,
    encoder_name: &str,
    encoder_args: &[String],
    vs: &mut VideoSource,
    scene: &SceneEntry,
    crop: Option<Crop>,
) -> Result<()> {
    let mut child = std::process::Command::new(encoder_bin)
        .args(encoder_args)
        .args(["--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start encoder '{encoder_name}'"))?;

    // A progress line per frame: a full stderr pipe deadlocks it against the Y4M writer.
    let mut enc_err = child.stderr.take().expect("encoder stderr unavailable");
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = enc_err.read_to_string(&mut s);
        s
    });

    let mut stdin = BufWriter::with_capacity(256 * 1024, child.stdin.take().expect("encoder stdin unavailable"));
    let write_res = vs.write_y4m_range(&mut stdin, scene.start_frame, scene.end_frame, crop);
    drop(stdin);

    let status = child.wait().context("wait for encoder")?;
    let stderr = err_t.join().unwrap_or_default();

    // Status first: an encoder that died early turns the write into a broken pipe.
    if !status.success() {
        bail!("encoder failed (chunk {:05}):\n{stderr}", scene.index + 1);
    }
    write_res.context("write Y4M frames to encoder")?;
    Ok(())
}

/// FFMS2 Y4M (cropped) piped through ffmpeg for scaling, then into the encoder.
fn encode_scaled(
    encoder_bin: &OsStr,
    encoder_name: &str,
    encoder_args: &[String],
    vs: &mut VideoSource,
    scene: &SceneEntry,
    crop: Option<Crop>,
    scale: (u32, u32),
) -> Result<()> {
    let mut ff = spawn_scaler(scale)?;

    let ff_out = ff.stdout.take().expect("ffmpeg stdout unavailable");
    let mut ff_err = ff.stderr.take().expect("ffmpeg stderr unavailable");

    let mut child = std::process::Command::new(encoder_bin)
        .args(encoder_args)
        .args(["--input", "-"])
        .stdin(Stdio::from(ff_out))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start encoder '{encoder_name}'"))?;
    let mut enc_err = child.stderr.take().expect("encoder stderr unavailable");

    // Drain both stderr pipes on threads so neither can block the pipeline.
    let ff_err_t = std::thread::spawn(move || { let mut s = String::new(); let _ = ff_err.read_to_string(&mut s); s });
    let enc_err_t = std::thread::spawn(move || { let mut s = String::new(); let _ = enc_err.read_to_string(&mut s); s });

    let mut ff_in = BufWriter::with_capacity(256 * 1024, ff.stdin.take().expect("ffmpeg stdin unavailable"));
    let write_res = vs.write_y4m_range(&mut ff_in, scene.start_frame, scene.end_frame, crop);
    drop(ff_in);

    let ff_status  = ff.wait().context("wait for ffmpeg scaler")?;
    let enc_status = child.wait().context("wait for encoder")?;
    let ff_stderr  = ff_err_t.join().unwrap_or_default();
    let enc_stderr = enc_err_t.join().unwrap_or_default();

    // Status first: a scaler or encoder that died early turns the write into a broken pipe.
    if !ff_status.success() {
        bail!("ffmpeg scaler failed (chunk {:05}):\n{ff_stderr}", scene.index + 1);
    }
    if !enc_status.success() {
        bail!("encoder failed (chunk {:05}):\n{enc_stderr}", scene.index + 1);
    }
    write_res.context("write Y4M frames to ffmpeg scaler")?;
    Ok(())
}

pub fn spawn_scaler((w, h): (u32, u32)) -> Result<std::process::Child> {
    let vf = format!("scale={w}:{h}:flags=lanczos");
    std::process::Command::new(external_bin("ffmpeg"))
        .args(["-hide_banner", "-loglevel", "error", "-f", "yuv4mpegpipe", "-i", "pipe:0"])
        // -strict -1: yuv4mpegpipe muxer needs it to write >8-bit Y4M.
        .args(["-vf", &vf, "-strict", "-1", "-f", "yuv4mpegpipe", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start ffmpeg scaler")
}

fn chunk_size(output_path: &Path) -> Result<u64> {
    let meta = std::fs::metadata(output_path)
        .with_context(|| format!("chunk output not found: {}", output_path.display()))?;
    if meta.len() == 0 {
        bail!("encoder produced empty file: {}", output_path.display());
    }
    Ok(meta.len())
}

fn encoder_binary(enc: Encoder) -> &'static str {
    match enc {
        Encoder::SvtAv1    => "SvtAv1EncApp",
        Encoder::SvtAv1Hdr => "SvtAv1EncApp-hdr",
    }
}

/// Replace a `--flag value` pair in place, or append it if absent.
fn set_arg(args: &mut Vec<String>, flag: &str, value: String) {
    match args.iter().position(|a| a == flag) {
        Some(i) if i + 1 < args.len() => args[i + 1] = value,
        _ => {
            args.push(flag.to_string());
            args.push(value);
        }
    }
}

fn build_encoder_args(config: &Config, output_path: &Path, opts: &EncodeOptions) -> Result<Vec<String>> {
    let out = output_path.to_str()
        .with_context(|| format!("non-UTF8 output path: {}", output_path.display()))?;

    let mut args = vec!["-b".to_string(), out.to_string()];
    args.extend(merged_encoder_args(config, opts));
    Ok(args)
}

/// Config args plus auto-HDR and auto-keyint, unless `encoder_params` already has the key.
pub fn merged_encoder_args(config: &Config, opts: &EncodeOptions) -> Vec<String> {
    let mut args = config.encoder_args();

    debug_assert_eq!(opts.hdr_args.len() % 2, 0, "hdr_args must contain flag-value pairs");
    for pair in opts.hdr_args.chunks(2) {
        if let [flag, value] = pair {
            let key = flag.trim_start_matches('-');
            if config.encoder_params.contains_key(key) {
                tracing::debug!("auto-HDR: skipping {flag} - overridden by encoder_params");
            } else {
                args.push(flag.clone());
                args.push(value.clone());
            }
        }
    }

    if let Some(keyint) = opts.keyint
        && !config.encoder_params.contains_key("keyint")
    {
        args.extend_from_slice(&["--keyint".into(), keyint.to_string()]);
    }

    args
}

/// Readable, and holding `expected_frames` when avet encoded it (None for `video = copy`).
pub fn validate_output(path: &Path, expected_frames: Option<u64>) -> Result<()> {
    const TIMEOUT_SECS: u64 = 300;

    let mut cmd = std::process::Command::new(external_bin("ffprobe"));
    cmd.args(["-v", "error", "-i"]).arg(path);
    let out = crate::ext::output_with_timeout(&mut cmd, TIMEOUT_SECS, "ffprobe output validation")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("output file is invalid: {stderr}");
    }

    // A merge that lost a chunk is valid Matroska that ends early.
    if let Some(expected) = expected_frames {
        let actual = video_packet_count(path)?;
        if actual != expected {
            bail!(
                "output holds {actual} video frames, but the chunk list accounts for \
                 {expected}. The merged video is short - delete the job's temp dir to \
                 encode it again from scratch."
            );
        }
    }
    Ok(())
}

/// Video frames in a muxed file. One ffprobe pass over the container, no decoding.
fn video_packet_count(path: &Path) -> Result<u64> {
    #[derive(serde::Deserialize)]
    struct Root { streams: Vec<Stream> }
    #[derive(serde::Deserialize)]
    struct Stream { nb_read_packets: Option<String> }

    // Walks the container, so the header-sized default would kill it on long encodes.
    const TIMEOUT_SECS: u64 = 3600;

    let root: Root = crate::ext::ffprobe_json_with_timeout(
        &["-v", "error", "-select_streams", "v:0", "-count_packets",
          "-show_entries", "stream=nb_read_packets", "-of", "json"],
        path,
        TIMEOUT_SECS,
    )
    .context("count the output's video frames")?;

    root.streams
        .into_iter()
        .next()
        .and_then(|s| s.nb_read_packets)
        .and_then(|s| s.parse().ok())
        .context("ffprobe reported no video frame count for the output")
}

/// Absolute, because the demuxer resolves against the list file's own directory, and
/// escaped for the concat parser, not the shell.
fn concat_entry(path: &Path) -> Result<String> {
    let abs = std::path::absolute(path)
        .with_context(|| format!("absolute path for {}", path.display()))?;
    let path_str = abs.to_str()
        .with_context(|| format!("non-UTF8 chunk path: {}", abs.display()))?;
    let escaped: String = path_str
        .chars()
        .flat_map(|c| {
            let escape = matches!(c, '\\' | '\'' | '"' | ' ' | '\t' | '#');
            escape.then_some('\\').into_iter().chain(std::iter::once(c))
        })
        .collect();
    Ok(format!("file {escaped}"))
}

pub fn concat_chunks(
    chunk_paths: &[PathBuf],
    output_path: &Path,
    list_dir: &Path,
) -> Result<()> {
    // Generous: a stream copy of the whole video. It bounds a wedge, not the runtime.
    const TIMEOUT_SECS: u64 = 3600;

    let list_path = list_dir.join("concat_list.txt");

    {
        let mut f = std::fs::File::create(&list_path).context("create concat_list.txt")?;
        for p in chunk_paths {
            writeln!(f, "{}", concat_entry(p)?).context("write concat_list.txt")?;
        }
    }

    let mut cmd = std::process::Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path)
        .args(["-c:v", "copy", "-map_metadata", "-1", "-an", "-sn"])
        .arg(output_path);
    let out = crate::ext::output_with_timeout(&mut cmd, TIMEOUT_SECS, "ffmpeg concat")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg concat failed:\n{stderr}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn params(pairs: &[(&str, i64)]) -> HashMap<String, toml::Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), toml::Value::Integer(*v))).collect()
    }

    fn cfg(encoder_params: HashMap<String, toml::Value>) -> Config {
        Config { encoder: Some(Encoder::SvtAv1), encoder_params, ..Default::default() }
    }

    #[test]
    fn concat_entry_is_absolute_and_escaped() {
        // A relative chunk path would be resolved against the list file's own directory.
        let line = concat_entry(Path::new("output/.avet_a/chunks/00001.ivf")).unwrap();
        let path = line.strip_prefix("file ").unwrap();
        assert!(path.starts_with('/'), "not absolute: {line}");

        let line = concat_entry(Path::new("/tmp/Movie \"Title\" #1/c.ivf")).unwrap();
        assert_eq!(line, "file /tmp/Movie\\ \\\"Title\\\"\\ \\#1/c.ivf");
    }

    #[test]
    fn build_encoder_args_includes_params() {
        let config = cfg(params(&[("crf", 28), ("preset", 6)]));
        let opts = EncodeOptions { fps_num: 24, fps_den: 1, ..Default::default() };

        let out = PathBuf::from("/tmp/chunk.ivf");
        let args = build_encoder_args(&config, &out, &opts).unwrap();

        assert!(args.contains(&"-b".to_string()));
        assert!(args.contains(&"--crf".to_string()));
        assert!(args.contains(&"28".to_string()));
        assert!(args.contains(&"--preset".to_string()));
        assert!(args.contains(&"6".to_string()));
    }

    #[test]
    fn auto_keyint_skipped_when_manual() {
        let config = cfg(params(&[("keyint", 240)]));
        let opts = EncodeOptions { keyint: Some(120), fps_num: 24, fps_den: 1, ..Default::default() };

        let out = PathBuf::from("/tmp/chunk.ivf");
        let args = build_encoder_args(&config, &out, &opts).unwrap();

        let keyint_pos = args.iter().position(|a| a == "--keyint").unwrap();
        assert_eq!(args[keyint_pos + 1], "240");
        assert_eq!(args.iter().filter(|a| *a == "--keyint").count(), 1);
    }

    #[test]
    fn auto_keyint_injected_when_not_manual() {
        let config = cfg(HashMap::new());
        let opts = EncodeOptions { keyint: Some(120), fps_num: 24, fps_den: 1, ..Default::default() };

        let out = PathBuf::from("/tmp/chunk.ivf");
        let args = build_encoder_args(&config, &out, &opts).unwrap();

        let keyint_pos = args.iter().position(|a| a == "--keyint").unwrap();
        assert_eq!(args[keyint_pos + 1], "120");
    }

    #[test]
    fn auto_hdr_skipped_when_manual_override() {
        let config = cfg(params(&[("color-primaries", 1)]));
        let opts = EncodeOptions {
            hdr_args: vec![
                "--color-primaries".into(), "9".into(),
                "--transfer-characteristics".into(), "16".into(),
            ],
            fps_num: 24,
            fps_den: 1,
            ..Default::default()
        };

        let args = merged_encoder_args(&config, &opts);
        let pos = args.iter().position(|a| a == "--color-primaries").unwrap();
        assert_eq!(args[pos + 1], "1");
        assert_eq!(args.iter().filter(|a| *a == "--color-primaries").count(), 1);
        assert!(args.windows(2).any(|w| w[0] == "--transfer-characteristics" && w[1] == "16"));
    }
}
