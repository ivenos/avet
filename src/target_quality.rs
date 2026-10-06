use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use crate::config::{Config, TargetQualityConfig};
use crate::encode::{self, EncodeOptions};
use crate::ext::{external_bin, reap};
use crate::ffms2::{Crop, OpenOpts, VideoSource};
use crate::resume::SceneEntry;

// CRF granularity of the SVT-AV1 encoders (and the HDR fork): quarter steps.
const CRF_STEP: f64 = 0.25;

// Seed the first step only; measured on 4K HDR SVT-AV1 near the target zone.
const NOMINAL_JOD_PER_CRF: f64 = 0.025;
const NOMINAL_LN_SIZE_PER_CRF: f64 = 0.07;

const CAMBI_PERCENTILE: f64 = 95.0;

/// The display CVVDP scores against. Vship takes pixels-per-degree from the model's own
/// resolution, never from the content, so the model has to carry the comparison's size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayModel {
    width: u32,
    height: u32,
    diagonal_inches: f64,
    hdr: bool,
}

/// A 16:9 panel of 30 inches at twice its height, the geometry of Vship's own standard models.
const DISPLAY_INCHES: f64 = 30.0;
const DISPLAY_DISTANCE_M: &str = "0.7472";

impl DisplayModel {
    /// Luminance as in Vship's own models. An SDR source measures ~2.5 JOD low against
    /// an HDR display, so the two are kept apart.
    pub fn config_json(&self) -> String {
        let (colorspace, max_luminance, contrast, ambient) = if self.hdr {
            ("HDR", "1500", "1000000", "10")
        } else {
            ("sRGB", "200", "1000", "250")
        };
        format!(
            "{{\"{}\":{{\"name\":\"avet\",\"resolution\":[{},{}],\"colorspace\":\"{colorspace}\",\
             \"viewing_distance_meters\":{DISPLAY_DISTANCE_M},\"diagonal_size_inches\":{:.3},\
             \"max_luminance\":{max_luminance},\"contrast\":{contrast},\"E_ambient\":{ambient},\
             \"k_refl\":0.005}}}}",
            Self::KEY, self.width, self.height, self.diagonal_inches
        )
    }

    /// The name the config is looked up under; without it FFVship keeps its own default.
    pub const KEY: &'static str = "avet";

    pub fn describe(&self) -> String {
        let kind = if self.hdr { "HDR" } else { "SDR" };
        format!("{}x{} {kind}", self.width, self.height)
    }
}

/// HDR by the signaled transfer, at the resolution the two files are compared at. `frame`
/// fills the panel as far as its shape allows, and a crop of it keeps the pixel size.
pub fn display_model_for(width: u32, height: u32, frame: (u32, u32), hdr_args: &[String]) -> DisplayModel {
    let hdr = matches!(signaled_transfer(hdr_args), Some("16" | "18"));
    let unit = DISPLAY_INCHES / 16f64.hypot(9.0);
    let pitch = (16.0 * unit / f64::from(frame.0)).min(9.0 * unit / f64::from(frame.1));
    let diagonal_inches = pitch * f64::from(width).hypot(f64::from(height));
    DisplayModel { width, height, diagonal_inches, hdr }
}

fn signaled_transfer(hdr_args: &[String]) -> Option<&str> {
    hdr_args
        .windows(2)
        .find(|w| w[0] == "--transfer-characteristics")
        .map(|w| w[1].as_str())
}

/// A Vulkan device reported by FFVship.
#[derive(Clone, Debug)]
pub struct GpuSelection {
    pub id: u32,
    pub label: String,
    /// False for a software rasterizer (llvmpipe); target_quality rejects those.
    pub hardware: bool,
}

impl GpuSelection {
    pub fn describe(&self) -> String {
        format!("gpu {} {}", self.id, self.label)
    }
}

/// A software Vulkan device is rejected: CVVDP on the CPU is too slow to be practical.
pub fn ensure_available() -> Result<GpuSelection> {
    const HINT: &str = "Pass /dev/dri for an Intel or AMD GPU; NVIDIA's driver loads only in the AppImage.";

    let mut cmd = std::process::Command::new(external_bin("FFVship"));
    cmd.arg("--list-gpu");
    let out = crate::ext::output_with_timeout(&mut cmd, 120, "FFVship --list-gpu")
        .context("is the FFVship tool bundled?")?;
    // With no usable Vulkan driver at all, FFVship aborts creating the instance.
    if !out.status.success() {
        bail!(
            "target_quality requires a GPU, but FFVship could not initialize Vulkan:\n{}\n{HINT}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let devices = list_gpus(&String::from_utf8_lossy(&out.stdout));
    let software = std::env::var_os("AVET_SOFTWARE_GPU").is_some_and(|v| v == "1");
    if let Some(g) = first_usable(&devices, software, passes_kernel_check) {
        return Ok(g.clone());
    }
    let hardware: Vec<&str> = devices.iter().filter(|d| d.hardware).map(|d| d.label.as_str()).collect();
    match devices.first() {
        _ if !hardware.is_empty() => bail!(
            "target_quality requires a GPU, but none passes Vship's own check: {}. {HINT}",
            hardware.join(", ")
        ),
        Some(g) => bail!(
            "target_quality requires a GPU, but FFVship found only a software Vulkan device ({}). {HINT}",
            g.label
        ),
        None => bail!("target_quality requires a GPU, but FFVship found no Vulkan device. {HINT}"),
    }
}

fn first_usable(devices: &[GpuSelection], software: bool, passes: impl Fn(u32) -> bool) -> Option<&GpuSelection> {
    devices.iter().find(|d| (d.hardware || software) && passes(d.id))
}

/// `--list-gpu` also lists devices Vship refuses to run on, such as one without 64-bit shader integers.
fn passes_kernel_check(id: u32) -> bool {
    let mut cmd = std::process::Command::new(external_bin("FFVship"));
    cmd.args(["--gpu-info", "--gpu-id", &id.to_string()]);
    crate::ext::output_with_timeout(&mut cmd, 120, "FFVship --gpu-info").is_ok_and(|out| {
        out.status.success() && String::from_utf8_lossy(&out.stdout).lines().any(|l| l.trim() == "Passes Kernel Check: 1")
    })
}

fn list_gpus(list: &str) -> Vec<GpuSelection> {
    let mut devices: Vec<GpuSelection> = Vec::new();
    for line in list.lines() {
        let Some(rest) = line.trim().strip_prefix("GPU ") else { continue };
        let Some((id_str, name)) = rest.split_once(':') else { continue };
        let Ok(id) = id_str.trim().parse::<u32>() else { continue };
        let label = name.trim().to_string();
        let low = label.to_lowercase();
        let hardware =
            !low.contains("llvmpipe") && !low.contains("software") && !low.contains("swrast");
        devices.push(GpuSelection { id, label, hardware });
    }
    devices
}

pub struct ProbeContext<'a> {
    pub source: &'a Path,
    pub index: &'a Path,
    pub temp_dir: &'a Path,
    pub config: &'a Config,
    pub opts: &'a EncodeOptions,
    pub tq: &'a TargetQualityConfig,
    /// HDR or SDR as first signaled, then as FFVship read the source.
    pub display_model: &'a Mutex<DisplayModel>,
    pub source_rgb: bool,
    pub gpu_id: u32,
    /// Held around FFVship: a second run on the same GPU adds VRAM, not throughput.
    pub gpu_lock: &'a Mutex<()>,
    /// Source dimensions, needed to turn avet crop (offset+size) into FFVship edge crops.
    pub source_width: u32,
    pub source_height: u32,
    pub n_threads: usize,
    pub stem: &'a str,
    /// Cumulative source byte sizes by frame (len = frames + 1); empty disables the cap.
    pub source_byte_index: &'a [u64],
}

#[derive(Clone, Copy)]
struct Probe {
    crf: f64,
    jod: f64,
    cambi: Option<Cambi>,
    size_pct: f64,
}

/// The encode's own CAMBI, and how far it lies above that of the encoder input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cambi {
    pub score: f64,
    pub diff: f64,
}

impl Probe {
    fn scores(&self) -> String {
        scores(self.jod, self.cambi)
    }
}

/// What a probe has to hold, the size cap aside.
#[derive(Clone, Copy)]
struct Floor {
    jod: f64,
    max_cambi: Option<f64>,
    max_cambi_diff: Option<f64>,
}

impl Floor {
    fn new(tq: &TargetQualityConfig) -> Self {
        Floor { jod: tq.jod, max_cambi: tq.max_cambi, max_cambi_diff: tq.max_cambi_diff }
    }

    fn needs_cambi(&self, jod: f64) -> bool {
        (self.max_cambi.is_some() || self.max_cambi_diff.is_some()) && jod >= self.jod
    }

    fn holds(&self, p: &Probe) -> bool {
        let under = |limit: Option<f64>, value: fn(&Cambi) -> f64| {
            limit.is_none_or(|m| p.cambi.is_some_and(|c| value(&c) <= m))
        };
        p.jod >= self.jod && under(self.max_cambi, |c| c.score) && under(self.max_cambi_diff, |c| c.diff)
    }
}

fn scores(jod: f64, cambi: Option<Cambi>) -> String {
    match cambi {
        Some(c) => format!("JOD {jod:.3}, CAMBI {:.2} (+{:.2})", c.score, c.diff),
        None    => format!("JOD {jod:.3}"),
    }
}

/// Why the search settled on its CRF, for logging.
pub enum SolveOutcome {
    /// Highest CRF that holds the floor within the size cap.
    Met,
    /// Size cap forced a higher CRF than the floor allowed; the floor is not held.
    CapBinding,
    /// Floor unreachable in the CRF range; best-quality probe used.
    FloorUnreachable,
}

pub struct SolveResult {
    pub crf: f64,
    pub jod: f64,
    pub cambi: Option<Cambi>,
    pub size_pct: f64,
    pub outcome: SolveOutcome,
}

impl SolveResult {
    pub fn scores(&self) -> String {
        scores(self.jod, self.cambi)
    }
}

pub fn solve_chunk_crf(ctx: &ProbeContext, scene: &SceneEntry) -> Result<SolveResult> {
    let lo = ctx.tq.min_crf as f64;
    let hi = ctx.tq.max_crf as f64;
    let key = scene.padded_index();
    let mut n = 0u32;

    solve(ctx.tq, seed_crf(ctx.config, lo, hi), &mut |crf| {
        let probe = probe_once(ctx, scene, crf)?;
        n += 1;
        tracing::info!(
            "[{}] chunk {key} probe {n}/{} crf {crf} gives {}, {:.0}% size",
            ctx.stem, ctx.tq.max_probes, probe.scores(), probe.size_pct
        );
        Ok(probe)
    })
}

/// Highest CRF holding the floor, or the lowest fitting the size cap where that lies above it.
/// A probe says "go up" while it holds the floor or is over the cap, and both fall with the CRF.
fn solve(
    tq: &TargetQualityConfig,
    seed: f64,
    probe_at: &mut dyn FnMut(f64) -> Result<Probe>,
) -> Result<SolveResult> {
    let lo = tq.min_crf as f64;
    let hi = tq.max_crf as f64;
    let floor = Floor::new(tq);
    let cap = tq.max_encoded_percent;

    let mut pts: Vec<Probe> = Vec::new();
    let mut crf = round_to_step(seed, lo, hi);
    loop {
        let probe = probe_at(crf)?;
        pts.push(probe);
        let n = pts.len() as u32;
        let settled = n >= tq.min_probes
            && floor.holds(&probe) && probe.jod <= floor.jod + tq.tolerance && probe.size_pct <= cap;
        if settled || n >= tq.max_probes {
            break;
        }
        match next_probe(&pts, &floor, cap, lo, hi, n + 1 == tq.max_probes) {
            Some(next) => crf = next,
            None => break,
        }
    }
    Ok(decide(&pts, &floor, cap, lo))
}

fn seed_crf(config: &Config, lo: f64, hi: f64) -> f64 {
    config
        .encoder_params
        .get("crf")
        .and_then(|v| match v {
            toml::Value::Integer(i) => Some(*i as f64),
            toml::Value::Float(f)   => Some(*f),
            toml::Value::String(s)  => s.parse().ok(),
            _ => None,
        })
        .unwrap_or((lo + hi) / 2.0)
        .clamp(lo, hi)
}

/// Both readings carry the probe preset's bias; the final encode can come out larger.
fn probe_once(ctx: &ProbeContext, scene: &SceneEntry, crf: f64) -> Result<Probe> {
    let tag = format!("{}_{crf}", scene.padded_index());
    let probe = ctx.temp_dir.join(format!("probe_{tag}.ivf"));
    let mut opts = EncodeOptions {
        dynamic_hdr: Default::default(), hdr10plus_frames: None, deinterlace: None, ..ctx.opts.clone()
    };
    // FFVship guesses an untagged matrix from the height, the source's uncropped one included.
    if !opts.hdr_args.iter().any(|a| a == "--matrix-coefficients") {
        let guess = if ctx.source_height > 650 { "1" } else { "5" };
        opts.hdr_args.extend(["--matrix-coefficients".to_string(), guess.to_string()]);
    }
    // It takes untagged RGB for sRGB with BT.709 primaries, and guesses both from the matrix on the encode.
    for (flag, srgb) in [("--transfer-characteristics", "13"), ("--color-primaries", "1")] {
        if ctx.source_rgb && !opts.hdr_args.iter().any(|a| a == flag) {
            opts.hdr_args.extend([flag.to_string(), srgb.to_string()]);
        }
    }
    let size_bytes = encode::encode_chunk(
        ctx.source,
        ctx.index,
        scene,
        &probe,
        ctx.config,
        &opts,
        encode::EncodeOverrides { crf: Some(crf), preset: Some(ctx.tq.probe_preset) },
    )
    // Nothing else in the temp dir's housekeeping knows about probe files.
    .inspect_err(|_| { let _ = std::fs::remove_file(&probe); })
    .with_context(|| format!("probe encode crf {crf}"))?;

    let jod = score(ctx, scene, &probe, &tag).and_then(|score| match score.mismatch {
        Some(readings) => bail!("FFVship reads {readings}, so its score would compare two different pictures"),
        None => Ok(score.jod),
    });
    let result = jod.and_then(|jod| {
        let cambi = Floor::new(ctx.tq).needs_cambi(jod)
            .then(|| measure_cambi(ctx, scene, &probe, &tag))
            .transpose()?;
        Ok((jod, cambi))
    });
    let _ = std::fs::remove_file(&probe);
    let (jod, cambi) = result.with_context(|| format!("measure chunk {:05} at crf {crf}", scene.index + 1))?;
    let size_pct = chunk_size_pct(size_bytes, ctx.source_byte_index, scene.start_frame, scene.end_frame);
    Ok(Probe { crf, jod, cambi, size_pct })
}

/// The finished chunk as it will play, which the probes at another preset only estimate.
pub fn measure_final(ctx: &ProbeContext, scene: &SceneEntry, chunk: &Path) -> Option<f64> {
    // FFVship would hold the deinterlaced encode against the source's combed frames.
    if ctx.opts.deinterlace.is_some() {
        return None;
    }
    match score(ctx, scene, chunk, &format!("{}_final", scene.padded_index())) {
        Ok(Score { jod, mismatch: None }) => Some(jod),
        Ok(Score { mismatch: Some(readings), .. }) => {
            tracing::debug!("[{}] chunk {}: not measured, FFVship reads {readings}", ctx.stem, scene.padded_index());
            None
        }
        Err(e) => {
            tracing::warn!("[{}] chunk {}: could not measure the finished encode: {e:#}", ctx.stem, scene.padded_index());
            None
        }
    }
}

struct Score {
    jod: f64,
    mismatch: Option<String>,
}

fn score(ctx: &ProbeContext, scene: &SceneEntry, distorted: &Path, tag: &str) -> Result<Score> {
    let _gpu = ctx.gpu_lock.lock().unwrap();
    let measured = |display_model| measure(&MeasureOpts {
        distorted,
        source: ctx.source,
        index: ctx.index,
        work_dir: ctx.temp_dir,
        start: scene.start_frame,
        crop: ctx.opts.crop,
        source_width: ctx.source_width,
        source_height: ctx.source_height,
        frames: scene.frame_count(),
        display_model,
        gpu_id: ctx.gpu_id,
        tag,
    });

    let model = *ctx.display_model.lock().unwrap();
    let (mut jod, readings) = measured(model)?;
    let Some((source, encoded)) = readings else {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| tracing::warn!("FFVship did not say how it read the files - its scores go unchecked"));
        return Ok(Score { jod, mismatch: None });
    };
    // Vship takes a BT.2020 matrix without a transfer for PQ, whatever the file signals.
    if source.hdr() != model.hdr {
        let model = DisplayModel { hdr: source.hdr(), ..model };
        let mut shared = ctx.display_model.lock().unwrap();
        if shared.hdr != model.hdr {
            tracing::info!("[{}] target quality: FFVship reads the source as {}, display {}", ctx.stem, source.transfer, model.describe());
            *shared = model;
        }
        drop(shared);
        jod = measured(model)?.0;
    }
    let mismatch = (!source.same_picture(&encoded)).then(|| format!("the source as {source} and the encode as {encoded}"));
    Ok(Score { jod, mismatch })
}

#[derive(Debug, Default, PartialEq)]
struct Reading {
    family: String,
    range: String,
    matrix: String,
    transfer: String,
    primaries: String,
}

impl Reading {
    fn hdr(&self) -> bool {
        matches!(self.transfer.as_str(), "PQ" | "HLG")
    }

    /// RGB reaches the encoder as limited-range BT.470BG YUV, so there only the light compares.
    fn same_picture(&self, encoded: &Reading) -> bool {
        self.transfer == encoded.transfer
            && self.primaries == encoded.primaries
            && (self.family == "RGB" || (self.matrix == encoded.matrix && self.range == encoded.range))
    }
}

impl std::fmt::Display for Reading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} range, matrix {}, transfer {}, primaries {}", self.family, self.range, self.matrix, self.transfer, self.primaries)
    }
}

fn parse_readings(stdout: &str) -> Option<(Reading, Reading)> {
    let (mut source, mut encoded) = (Reading::default(), Reading::default());
    let mut current: Option<&mut Reading> = None;
    for line in stdout.lines() {
        if line.contains("Source-Colorspace") {
            current = Some(&mut source);
        } else if line.contains("Encoded-Colorspace") {
            current = Some(&mut encoded);
        } else if let (Some(reading), Some((key, value))) = (current.as_deref_mut(), line.split_once(": ")) {
            let field = match key.trim() {
                "Color Family" => &mut reading.family,
                "Range" => &mut reading.range,
                "YUV Matrix" => &mut reading.matrix,
                "Transfer Function" => &mut reading.transfer,
                "Primaries" => &mut reading.primaries,
                _ => continue,
            };
            *field = value.trim().to_string();
        }
    }
    let complete = |r: &Reading| ![&r.family, &r.range, &r.matrix, &r.transfer, &r.primaries].iter().any(|v| v.is_empty());
    (complete(&source) && complete(&encoded)).then_some((source, encoded))
}

/// Percent of the source's own bytes for these frames; 0 when the index is missing.
pub fn chunk_size_pct(encoded: u64, cum: &[u64], start: u64, end: u64) -> f64 {
    match (cum.get(start as usize), cum.get(end as usize + 1)) {
        (Some(lo), Some(hi)) if hi > lo => encoded as f64 / (hi - lo) as f64 * 100.0,
        _ => 0.0,
    }
}

/// A CRF strictly inside the bracket, or None once the bracket is one step wide.
fn next_probe(pts: &[Probe], floor: &Floor, cap: f64, lo: f64, hi: f64, last: bool) -> Option<f64> {
    let go_up = |p: &Probe| floor.holds(p) || p.size_pct > cap;
    // Every probe lies inside the bracket of its time, so sorted by CRF the two kinds never mix.
    let mut by_crf = pts.to_vec();
    by_crf.sort_by(|x, y| x.crf.total_cmp(&y.crf));
    let split = by_crf.iter().filter(|p| go_up(p)).count();
    let below = split.checked_sub(1).map(|i| by_crf[i]);
    let above = by_crf.get(split).copied();
    // An end no probe has set yet lies one step outside the range, which leaves the bound a candidate.
    let a = below.map_or(lo - CRF_STEP, |p| p.crf);
    let b = above.map_or(hi + CRF_STEP, |p| p.crf);
    if b - a <= CRF_STEP + 1e-9 {
        return None;
    }

    // The line through the two bracket ends, else through the two probes nearest the open end.
    let i = split.saturating_sub(1).min(by_crf.len().saturating_sub(2));
    let (p, q) = (by_crf[i], by_crf.get(i + 1));
    let open_end = if above.is_none() { f64::INFINITY } else { f64::NEG_INFINITY };
    let crossing = |value: fn(&Probe) -> f64, nominal: f64, target: f64| {
        let slope = q.map_or(nominal, |q| (value(q) - value(&p)) / (q.crf - p.crf));
        if slope < -1e-9 { p.crf + (target - value(&p)) / slope } else { open_end }
    };
    // Each estimate rounds toward the side its constraint can pick.
    let c_floor = match (below, above) {
        (Some(l), _) if !floor.holds(&l) => None,
        // Banding failed the upper end, and it has no slope to follow.
        (_, Some(u)) if u.jod >= floor.jod => Some((a + b) / 2.0),
        _ => Some((crossing(|p| p.jod, -NOMINAL_JOD_PER_CRF, floor.jod) / CRF_STEP + 1e-6).floor() * CRF_STEP),
    };
    let c_cap = below.filter(|l| l.size_pct > cap).map(|_| {
        (crossing(|p| p.size_pct.ln(), -NOMINAL_LN_SIZE_PER_CRF, cap.ln()) / CRF_STEP - 1e-6).ceil() * CRF_STEP
    });
    let guess = c_floor.into_iter().chain(c_cap).fold(f64::NEG_INFINITY, f64::max);

    // A bracket with both ends probed may lag bisection by one probe, except on the last one.
    let room = ((hi - lo + 2.0 * CRF_STEP) / 2f64.powi(pts.len() as i32 - 1)).max((b - a) / 2.0);
    let paced = below.is_some() && above.is_some() && !last;
    let next = if paced { guess.clamp(b - room, a + room) } else { guess };
    Some(round_to_step(next, a + CRF_STEP, b - CRF_STEP))
}

fn round_to_step(v: f64, lo: f64, hi: f64) -> f64 {
    ((v / CRF_STEP).round() * CRF_STEP).clamp(lo, hi)
}

/// Highest CRF holding both, else the floor gives way to the cap, else the smallest chunk.
fn decide(pts: &[Probe], floor: &Floor, cap: f64, lo: f64) -> SolveResult {
    let res = |p: Probe, outcome| SolveResult {
        crf: p.crf, jod: p.jod, cambi: p.cambi, size_pct: p.size_pct, outcome,
    };
    if let Some(p) = pts.iter()
        .filter(|p| floor.holds(p) && p.size_pct <= cap)
        .max_by(|a, b| a.crf.total_cmp(&b.crf))
        .copied()
    {
        return res(p, SolveOutcome::Met);
    }

    // Nothing holds both, so the cap wins: lowest CRF that fits is the best quality left.
    // The search does not go below a probe over the cap, so only min_crf proves the floor out of reach.
    if let Some(p) = pts.iter()
        .filter(|p| p.size_pct <= cap)
        .min_by(|a, b| a.crf.total_cmp(&b.crf))
        .copied()
    {
        let out_of_reach = !pts.iter().any(|p| floor.holds(p)) && pts.iter().any(|p| p.crf <= lo + 1e-9);
        let outcome = if out_of_reach || !pts.iter().any(|p| p.size_pct > cap) {
            SolveOutcome::FloorUnreachable
        } else {
            SolveOutcome::CapBinding
        };
        return res(p, outcome);
    }

    // Every probe over the cap: the smallest chunk is the closest thing to honoring it.
    pts.iter()
        .min_by(|a, b| a.size_pct.total_cmp(&b.size_pct))
        .map(|&p| res(p, SolveOutcome::CapBinding))
        .unwrap_or(SolveResult {
            crf: lo, jod: f64::NAN, cambi: None, size_pct: f64::NAN,
            outcome: SolveOutcome::FloorUnreachable,
        })
}

struct MeasureOpts<'a> {
    distorted: &'a Path,
    source: &'a Path,
    /// avet's existing FFMS2 index for the source, reused read-only by FFVship.
    index: &'a Path,
    work_dir: &'a Path,
    /// First source frame of the chunk; the probe holds those frames from 0.
    start: u64,
    frames: u64,
    crop: Option<Crop>,
    source_width: u32,
    source_height: u32,
    display_model: DisplayModel,
    gpu_id: u32,
    /// Unique suffix for the per-measurement json file.
    tag: &'a str,
}

/// Removes its paths on drop so the json never lingers after a measurement.
struct Cleanup(Vec<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A driver reset, a GPU in use by something else or a lost device all come back on their
/// own; anything else FFVship reports is a verdict on the file.
fn gpu_error(what: &str, status: std::process::ExitStatus, stderr: &str) -> anyhow::Error {
    let err = crate::ext::tool_error(what, status, stderr);
    if err.downcast_ref::<crate::job::Transient>().is_none() && gpu_side(stderr) {
        return err.context(crate::job::Transient);
    }
    err
}

/// Vship reports as `VshipException`, then `<type>: <text>`. The other types are about the input.
fn gpu_side(stderr: &str) -> bool {
    const DEVICE: [&str; 7] = [
        "OutOfVRAM", "OutOfRAM", "InternalError", "DeviceCountError", "NoDeviceDetected", "BadDeviceArgument",
        "BadDeviceCode",
    ];
    let mut lines = stderr.lines();
    let reported = std::iter::from_fn(|| {
        lines.find(|l| l.trim() == "VshipException")?;
        lines.next()
    })
    .any(|l| l.split_once(':').is_some_and(|(kind, _)| DEVICE.contains(&kind.trim())));
    // FFVship's own check of the buffer it asks Vship for, which names no type.
    reported || stderr.contains("Pinned buffer allocation failed")
}

/// FFVship crops the source to match and resizes on a mismatch; its last cumulative
/// JOD is the chunk score.
fn measure(m: &MeasureOpts) -> Result<(f64, Option<(Reading, Reading)>)> {
    let json = m.work_dir.join(format!("cvvdp_{}.json", m.tag));
    let _cleanup = Cleanup(vec![json.clone()]);

    let mut cmd = std::process::Command::new(external_bin("FFVship"));
    cmd.arg("-s").arg(m.source)
        .arg("-e").arg(m.distorted)
        .arg("--source-index").arg(m.index)
        .args(measure_args(m))
        .arg("--json").arg(&json);

    // A wedged GPU takes the worker with it, and nothing above would notice.
    let out = crate::ext::output_with_timeout(&mut cmd, 1800 + m.frames, "FFVship")?;
    if !out.status.success() {
        return Err(gpu_error("FFVship", out.status, &String::from_utf8_lossy(&out.stderr)));
    }

    let jod = parse_cvvdp(&read_metric_json(&json, "FFVship")?)?;
    Ok((jod, parse_readings(&String::from_utf8_lossy(&out.stdout))))
}

/// Everything but the file paths. The probe holds the chunk's frames from 0, and avet's
/// crop is offset and size where FFVship wants per-edge amounts.
fn measure_args(m: &MeasureOpts) -> Vec<String> {
    let mut args: Vec<String> = ["-m", "CVVDP"].iter().map(|s| (*s).to_string()).collect();
    args.extend([
        "--start".into(), m.start.to_string(),
        "--encoded-offset".into(), format!("-{}", m.start),
        "--displayModel".into(), DisplayModel::KEY.to_string(),
        "--displayConfig".into(), m.display_model.config_json(),
        "--gpu-id".into(), m.gpu_id.to_string(),
        "--verbose".into(),
    ]);
    if let Some(c) = m.crop {
        args.extend([
            "--cropLeftSource".into(), c.x.to_string(),
            "--cropTopSource".into(), c.y.to_string(),
            "--cropRightSource".into(), m.source_width.saturating_sub(c.x + c.w).to_string(),
            "--cropBottomSource".into(), m.source_height.saturating_sub(c.y + c.h).to_string(),
        ]);
    }
    args
}

fn read_metric_json(path: &Path, what: &str) -> Result<String> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {what} json: {}", path.display()))?;
    if serde_json::from_str::<serde::de::IgnoredAny>(&raw).is_err_and(|e| e.is_eof()) {
        return Err(anyhow::Error::new(crate::job::Transient)
            .context(format!("{what} left {} empty or cut short - is the disk full?", path.display())));
    }
    Ok(raw)
}

/// CVVDP JSON is `[[cum], [cum], ...]`; the last row is the whole clip's score.
fn parse_cvvdp(raw: &str) -> Result<f64> {
    let rows: Vec<Vec<f64>> = serde_json::from_str(raw).context("parse FFVship CVVDP json")?;
    rows.last()
        .and_then(|r| r.last())
        .copied()
        .ok_or_else(|| anyhow!("FFVship CVVDP json had no scores"))
}

/// libvmaf's full-reference CAMBI clamps the per-frame diff at 0, so banding the source
/// already had costs nothing there.
fn measure_cambi(ctx: &ProbeContext, scene: &SceneEntry, probe: &Path, tag: &str) -> Result<Cambi> {
    let timeout_secs = 1800 + scene.frame_count();

    // Not in the temp dir: a share without Unix extensions has no FIFOs.
    let fifo = std::env::temp_dir().join(format!("avet_{}_cambi_{tag}.y4m", std::process::id()));
    let json = ctx.temp_dir.join(format!("cambi_{tag}.json"));
    let _ = std::fs::remove_file(&fifo);
    let _cleanup = Cleanup(vec![fifo.clone(), json.clone()]);
    make_fifo(&fifo)?;
    // O_RDWR on a Linux FIFO never blocks, so neither child hangs opening its end, and
    // dropping it is what lets vmaf see EOF, or the decoder EPIPE, once the other is gone.
    let hold = std::fs::OpenOptions::new().read(true).write(true).open(&fifo)
        .with_context(|| format!("open {}", fifo.display()))?;

    let mut cmd = std::process::Command::new(external_bin("vmaf"));
    cmd.args(["--reference", "/dev/stdin", "--distorted"]).arg(&fifo)
        .args(["--no_prediction", "--threads", &ctx.n_threads.to_string()])
        .args(["--feature", cambi_feature(&ctx.opts.hdr_args), "--json", "--output"]).arg(&json)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut vmaf = crate::ext::spawn(&mut cmd, "vmaf")?;
    let mut cmd = std::process::Command::new(external_bin("ffmpeg"));
    // CAMBI only scores flat areas, and synthesized grain leaves none: it would read 0.
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-export_side_data", "film_grain", "-i"]).arg(probe)
        .args(["-map", "0:v:0", "-fps_mode", "passthrough", "-strict", "-1", "-f", "yuv4mpegpipe"]).arg(&fifo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut decoder = match crate::ext::spawn(&mut cmd, "ffmpeg probe decoder") {
        Ok(c) => c,
        Err(e) => {
            reap(&mut vmaf);
            return Err(e);
        }
    };

    let sink = vmaf.stdin.take().expect("reference input is piped");
    let (source, index, opts) = (ctx.source.to_path_buf(), ctx.index.to_path_buf(), ctx.opts.clone());
    let (start, end) = (scene.start_frame, scene.end_frame);
    let writer = std::thread::spawn(move || -> Result<()> {
        let mut vs = VideoSource::open(&source, &index, OpenOpts { target_bit_depth: opts.target_bit_depth, keep_subsampling: false })
            .context("open FFMS2 VideoSource")?;
        vs.info.fps_num = opts.fps_num;
        vs.info.fps_den = opts.fps_den;
        let mut out = std::io::BufWriter::with_capacity(256 * 1024, sink);
        vs.write_y4m_range(&mut out, start, end, opts.crop, None)
    });

    let vmaf_err = crate::ext::drain_text(vmaf.stderr.take());
    let dec_err = crate::ext::drain_text(decoder.stderr.take());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut hold = Some(hold);
    let (mut vmaf_status, mut dec_status) = (None, None);
    while vmaf_status.is_none() || dec_status.is_none() {
        if vmaf_status.is_none() {
            vmaf_status = vmaf.try_wait().context("wait for vmaf")?;
        }
        if dec_status.is_none() {
            dec_status = decoder.try_wait().context("wait for ffmpeg probe decoder")?;
        }
        if vmaf_status.is_some() || dec_status.is_some() {
            hold = None;
        }
        if vmaf_status.is_none() || dec_status.is_none() {
            if std::time::Instant::now() >= deadline {
                reap(&mut vmaf);
                reap(&mut decoder);
                return Err(anyhow::Error::new(crate::job::Transient)
                    .context(format!("vmaf did not finish within {timeout_secs}s - killed")));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    drop(hold);
    let write_res = writer.join().unwrap_or_else(|_| Err(anyhow!("Y4M writer panicked")));
    let (vmaf_err, dec_err) = (vmaf_err.join(), dec_err.join());

    // Status first: any of them dying turns the others' writes into a broken pipe. vmaf
    // scores a decoder that stopped early on the frames it got, so its success is not enough.
    let failed: Vec<anyhow::Error> = [
        ("ffmpeg probe decoder", dec_status, dec_err),
        ("vmaf", vmaf_status, vmaf_err),
    ]
    .into_iter()
    .filter_map(|(what, status, stderr)| {
        let status = status.filter(|s| !s.success())?;
        Some(crate::ext::tool_error(what, status, &stderr.unwrap_or_default()))
    })
    .collect();
    if !failed.is_empty() {
        let transient = failed.iter().any(|e| e.downcast_ref::<crate::job::Transient>().is_some());
        let killed = failed.iter().any(|e| e.downcast_ref::<crate::job::Killed>().is_some());
        let mut err = anyhow!("{}", failed.iter().map(|e| e.root_cause().to_string()).collect::<Vec<_>>().join("\n"));
        if let Some(cause) = encode::feed_failure(write_res) {
            err = err.context(format!("reading the source failed first: {cause}"));
        }
        if killed {
            err = err.context(crate::job::Killed);
        }
        return Err(if transient { err.context(crate::job::Transient) } else { err });
    }
    write_res.context("write Y4M reference to vmaf")?;

    parse_cambi(&read_metric_json(&json, "vmaf")?)
}

/// CAMBI's visibility thresholds come in BT.1886 and PQ; HLG is measured as BT.1886.
fn cambi_feature(hdr_args: &[String]) -> &'static str {
    match signaled_transfer(hdr_args) {
        Some("16") => "cambi=full_ref=true:eotf=pq",
        _ => "cambi=full_ref=true",
    }
}

fn parse_cambi(raw: &str) -> Result<Cambi> {
    #[derive(serde::Deserialize)]
    struct Root { frames: Vec<Frame> }
    #[derive(serde::Deserialize)]
    struct Frame { metrics: HashMap<String, f64> }

    let root: Root = serde_json::from_str(raw).context("parse vmaf CAMBI json")?;
    let (mut scores, mut diffs) = (Vec::new(), Vec::new());
    for frame in &root.frames {
        let m = &frame.metrics;
        diffs.push(*m.get("cambi_full_reference").context("vmaf json has no cambi_full_reference")?);
        // libvmaf appends non-default options to every one of these names: the encode's
        // own score becomes `cambi_eotf_pq`, and the other two can follow.
        let mut found = m.iter().filter(|(k, _)| {
            k.starts_with("cambi")
                && !k.starts_with("cambi_source")
                && !k.starts_with("cambi_full_reference")
        });
        let score = found.next().context("vmaf json has no CAMBI score")?;
        if found.next().is_some() {
            bail!("vmaf json has more than one CAMBI score, so none of them can be read");
        }
        scores.push(*score.1);
    }
    if scores.is_empty() {
        bail!("vmaf json has no CAMBI frames");
    }
    Ok(Cambi { score: worst_frames(&mut scores), diff: worst_frames(&mut diffs) })
}

fn worst_frames(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let rank = (values.len() as f64 * CAMBI_PERCENTILE / 100.0).ceil() as usize;
    values[rank.clamp(1, values.len()) - 1]
}

fn make_fifo(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> std::os::raw::c_int;
    }
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("NUL byte in path: {}", path.display()))?;
    if unsafe { mkfifo(cpath.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("create fifo {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(crf: f64, jod: f64, size_pct: f64) -> Probe {
        Probe { crf, jod, cambi: None, size_pct }
    }

    fn pc(crf: f64, jod: f64, score: f64, diff: f64) -> Probe {
        Probe { crf, jod, cambi: Some(Cambi { score, diff }), size_pct: 50.0 }
    }

    fn jod(jod: f64) -> Floor {
        Floor { jod, max_cambi: None, max_cambi_diff: None }
    }

    fn cambi(max_cambi: Option<f64>, max_cambi_diff: Option<f64>) -> Floor {
        Floor { jod: 9.5, max_cambi, max_cambi_diff }
    }

    #[test]
    fn a_gpu_that_breaks_mid_job_is_retried_and_a_bad_file_is_not() {
        use std::os::unix::process::ExitStatusExt;
        let (aborted, exit1) = (std::process::ExitStatus::from_raw(6), std::process::ExitStatus::from_raw(1 << 8));
        let transient = |status, stderr| gpu_error("FFVship", status, stderr).downcast_ref::<crate::job::Transient>().is_some();

        assert!(transient(exit1, "VshipException\nInternalError: A GPU Call failed inside Vship. This may be due to a bad environment but is likely due to a bug in Vship.\n - At line 923 of src/Vulkan/butter/../util/vulkanHelper.hpp\nDetail: failed to create vulkan instance!\n"));
        assert!(transient(exit1, "VshipException\nBadDeviceArgument: Vship received a bad gpu_id argument either you specified a number >= to your gpu count, either it was negative\n - At line 184 of src/Vulkan/butter/../util/vulkanDeviceManager.hpp\n"));
        assert!(transient(aborted, "VshipException\nOutOfVRAM: Vship was not able to perform GPU memory allocation. (Advice) Reduce or Set numStream argument\n - At line 43 of csf.hpp\n\nAssertion failed!\nMessage    : Failed to initialize GPU Worker"));
        assert!(transient(aborted, "Assertion failed!\nMessage    : Pinned buffer allocation failed in allocate_external_rgb_buffer"));

        assert!(!transient(aborted, "VshipException\nBadDisplayModel: Vship was not able to find a corresponding model as specified.\n - At line 1 of x.hpp\nDetail: Display Unset, wrong display name?\n\nAssertion failed!\nMessage    : Failed to initialize GPU Worker"));
        assert!(!transient(aborted, "VshipException\nBadJson: Vship failed to parse the json\nDetail: OutOfVRAM: not a type here"));
        assert!(!transient(aborted, "Assertion failed!\nExpression : indexer != nullptr\nMessage    : FFMS2: Failed to create indexer for file [nope.ivf] - Can't open 'nope.ivf'"));
        assert!(!transient(exit1, "Error: could not open the distorted file"));
    }

    #[test]
    fn display_model_follows_the_signaled_transfer() {
        let args = |t: &str| vec!["--transfer-characteristics".to_string(), t.to_string()];

        assert_eq!(display_model_for(3840, 2160, (3840, 2160), &args("16")).describe(), "3840x2160 HDR");
        assert_eq!(display_model_for(3840, 2160, (3840, 2160), &args("18")).describe(), "3840x2160 HDR");
        assert_eq!(display_model_for(1280, 720, (1280, 720), &args("18")).describe(), "1280x720 HDR");

        // An SDR source signals bt709; an HDR display costs it ~2.5 JOD.
        assert_eq!(display_model_for(1920, 1080, (1920, 1080), &args("1")).describe(), "1920x1080 SDR");
        assert_eq!(display_model_for(3840, 2160, (3840, 2160), &[]).describe(), "3840x2160 SDR");
    }

    #[test]
    fn the_display_config_carries_the_comparison_resolution() {
        let pq = vec!["--transfer-characteristics".to_string(), "16".to_string()];

        // Vship reads pixels-per-degree off the model, so this is what fixes the reading.
        let hdr = display_model_for(1920, 1080, (1920, 1080), &pq).config_json();
        assert!(hdr.contains("\"resolution\":[1920,1080]"), "{hdr}");
        assert!(hdr.contains("\"colorspace\":\"HDR\""), "{hdr}");
        assert!(hdr.contains("\"max_luminance\":1500"), "{hdr}");
        assert!(hdr.starts_with(&format!("{{\"{}\":", DisplayModel::KEY)), "{hdr}");

        let sdr = display_model_for(3840, 2160, (3840, 2160), &[]).config_json();
        assert!(sdr.contains("\"resolution\":[3840,2160]"), "{sdr}");
        assert!(sdr.contains("\"colorspace\":\"sRGB\""), "{sdr}");
        assert!(sdr.contains("\"max_luminance\":200"), "{sdr}");

        // Vship errors out on a model that is missing any of these.
        for key in ["viewing_distance_meters", "diagonal_size_inches", "contrast", "E_ambient"] {
            assert!(sdr.contains(key), "{key} missing from {sdr}");
        }
        assert!(!sdr.contains('\n'), "the config goes through argv as one token");
    }

    #[test]
    fn a_crop_keeps_the_pixels_of_the_uncropped_frame() {
        let pitch = |m: DisplayModel| m.diagonal_inches / f64::from(m.width).hypot(f64::from(m.height));
        let full = display_model_for(1920, 1080, (1920, 1080), &[]);
        assert!((full.diagonal_inches - 30.0).abs() < 1e-9);
        assert!(full.config_json().contains("\"diagonal_size_inches\":30.000"), "{}", full.config_json());

        for (w, h) in [(1920, 800), (1440, 1080), (1440, 800)] {
            let cropped = display_model_for(w, h, (1920, 1080), &[]);
            assert!((pitch(cropped) - pitch(full)).abs() < 1e-12, "{w}x{h}");
        }

        // The same picture without the bars a crop would have removed.
        for (w, h) in [(1440, 1080), (1920, 800)] {
            let native = display_model_for(w, h, (w, h), &[]);
            assert!((pitch(native) - pitch(full)).abs() < 1e-12, "{w}x{h}");
        }
    }

    #[test]
    fn the_first_hardware_gpu_that_passes_is_chosen() {
        let all = |_: u32| true;
        let devices = list_gpus("GPU 0: NVIDIA GeForce RTX 5060 Ti\nGPU 1: llvmpipe (LLVM 22.1.7, 256 bits)\n");
        assert_eq!(first_usable(&devices, false, all).map(|g| g.id), Some(0));

        let devices = list_gpus("GPU 0: llvmpipe (LLVM 22.1.7)\nGPU 1: Intel Graphics\n");
        assert_eq!(first_usable(&devices, false, all).map(|g| g.id), Some(1));
        assert_eq!(first_usable(&devices, true, all).map(|g| g.id), Some(0));

        let devices = list_gpus("GPU 0: Intel(R) HD Graphics 4600 (HSW GT2)\nGPU 1: AMD Radeon RX 7600 (RADV NAVI33)\n");
        assert_eq!(first_usable(&devices, false, |id| id != 0).map(|g| g.id), Some(1));
        assert!(first_usable(&devices, false, |_| false).is_none());

        let software = list_gpus("GPU 0: llvmpipe (LLVM 22.1.7)\n");
        assert_eq!(software.len(), 1);
        assert!(!software[0].hardware);
        assert!(first_usable(&software, false, all).is_none());
    }

    #[test]
    fn a_metric_json_left_empty_or_cut_short_is_retried() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m.json");
        for (text, transient) in [("", true), ("[[9.9],[9.8", true), ("[[9.9],[9.8]]", false), ("{\"frames\": 1}", false)] {
            std::fs::write(&path, text).unwrap();
            let res = read_metric_json(&path, "FFVship");
            assert_eq!(res.as_ref().err().is_some_and(crate::job::is_transient), transient, "{text:?}");
        }
    }

    #[test]
    fn parse_cvvdp_takes_last_cumulative() {
        assert!((parse_cvvdp("[[9.95],[9.90],[9.83]]").unwrap() - 9.83).abs() < 1e-9);
        assert!(parse_cvvdp("[]").is_err());
    }

    /// As FFVship prints it, between its index and progress lines.
    fn verbose(source: [&str; 5], encoded: [&str; 5]) -> String {
        let block = |r: [&str; 5]| format!(
            "Source Size: 320x240\nSample Type: Uint8_t\nColor Family: {}\nRange: {}\nSubsampling (log): 1x1\nChroma Location: Left\n\
             YUV Matrix: {}\nTransfer Function: {}\nPrimaries: {}\n", r[0], r[1], r[2], r[3], r[4]
        );
        format!(
            "Successfully read index from [src.ffindex]\n-------Source-Colorspace--------\n{}------Encoded-Colorspace--------\n{}[|||] 12/12",
            block(source), block(encoded)
        )
    }

    #[test]
    fn ffvships_own_reading_of_the_two_files_is_what_is_compared() {
        let sdr = ["YUV", "Limited", "BT709", "BT709", "BT709"];
        let (source, encoded) = parse_readings(&verbose(sdr, sdr)).unwrap();
        assert!(source.same_picture(&encoded) && !source.hdr());
        assert_eq!(source.to_string(), "YUV Limited range, matrix BT709, transfer BT709, primaries BT709");

        // A crop under 650 rows, read with the other matrix; full range lost on the way.
        for other in [["YUV", "Limited", "BT470_BG", "BT470_BG", "BT470_BG"], ["YUV", "Full", "BT709", "BT709", "BT709"]] {
            let (source, encoded) = parse_readings(&verbose(sdr, other)).unwrap();
            assert!(!source.same_picture(&encoded), "{other:?}");
        }

        let pq = ["YUV", "Limited", "BT2020_NCL", "PQ", "BT2020"];
        assert!(parse_readings(&verbose(pq, pq)).unwrap().0.hdr());
        assert!(parse_readings(&verbose(["YUV", "Limited", "BT2020_NCL", "HLG", "BT2020"], pq)).unwrap().0.hdr());

        let rgb = ["RGB", "Full", "RGB", "sRGB", "BT709"];
        let (source, encoded) = parse_readings(&verbose(rgb, ["YUV", "Limited", "BT470_BG", "sRGB", "BT709"])).unwrap();
        assert!(source.same_picture(&encoded));
        let (source, encoded) = parse_readings(&verbose(rgb, ["YUV", "Limited", "BT470_BG", "BT470_BG", "BT470_BG"])).unwrap();
        assert!(!source.same_picture(&encoded));

        assert_eq!(parse_readings("Successfully read index from [src.ffindex]\n[|||] 12/12"), None);
        assert_eq!(parse_readings("-------Source-Colorspace--------\nRange: Limited\n"), None);
    }

    #[test]
    fn round_to_step_quarters_and_clamps() {
        assert_eq!(round_to_step(28.1, 14.0, 45.0), 28.0);
        assert_eq!(round_to_step(28.2, 14.0, 45.0), 28.25);
        assert_eq!(round_to_step(28.4, 14.0, 45.0), 28.5);
        assert_eq!(round_to_step(10.0, 14.0, 45.0), 14.0);
        assert_eq!(round_to_step(99.0, 14.0, 45.0), 45.0);
    }

    #[test]
    fn chunk_size_pct_uses_actual_source_bytes() {
        let cum = [0u64, 100, 300, 600, 1000];
        assert_eq!(chunk_size_pct(300, &cum, 0, 2), 50.0);
        assert_eq!(chunk_size_pct(200, &cum, 3, 3), 50.0);
        assert_eq!(chunk_size_pct(300, &cum, 0, 99), 0.0);
        assert_eq!(chunk_size_pct(300, &[], 0, 2), 0.0);
    }

    #[test]
    fn decide_picks_highest_crf_above_floor() {
        let pts = vec![p(30.0, 9.6, 50.0), p(32.0, 9.52, 45.0), p(36.0, 9.5, 40.0), p(40.0, 9.2, 35.0)];
        let r = decide(&pts, &jod(9.5), 90.0, 14.0);
        assert_eq!(r.crf, 36.0);
        assert!(matches!(r.outcome, SolveOutcome::Met));
    }

    #[test]
    fn decide_cap_binds_over_floor() {
        // floor CRF (20) is over the size cap; a higher CRF (25) is under it -> cap wins
        let pts = vec![p(20.0, 9.6, 120.0), p(25.0, 9.3, 80.0)];
        let r = decide(&pts, &jod(9.5), 90.0, 14.0);
        assert_eq!(r.crf, 25.0);
        assert!(matches!(r.outcome, SolveOutcome::CapBinding));
    }

    #[test]
    fn decide_floor_unreachable_uses_best_quality() {
        // all below floor -> highest jod (crf 30)
        let pts = vec![p(30.0, 9.0, 50.0), p(35.0, 8.8, 40.0), p(40.0, 8.5, 30.0)];
        let r = decide(&pts, &jod(9.5), 90.0, 14.0);
        assert_eq!(r.crf, 30.0);
        assert!(matches!(r.outcome, SolveOutcome::FloorUnreachable));
    }

    #[test]
    fn seed_uses_encoder_crf_when_present() {
        let mut config = Config {
            encoder: Some(crate::config::Encoder::SvtAv1),
            ..Default::default()
        };
        config.encoder_params.insert("crf".into(), toml::Value::Integer(28));
        assert_eq!(seed_crf(&config, 14.0, 45.0), 28.0);
        config.encoder_params.insert("crf".into(), toml::Value::Integer(60));
        assert_eq!(seed_crf(&config, 14.0, 45.0), 45.0);
        config.encoder_params.clear();
        assert_eq!(seed_crf(&config, 18.0, 44.0), 31.0);
    }

    #[test]
    fn decide_floor_unreachable_still_respects_the_cap() {
        // The best-quality probe is four times the size of the source.
        let pts = vec![p(1.0, 9.0, 398.0), p(20.0, 8.8, 120.0), p(35.0, 8.5, 60.0)];
        let r = decide(&pts, &jod(9.5), 90.0, 14.0);
        assert_eq!(r.crf, 35.0);
        assert!(matches!(r.outcome, SolveOutcome::FloorUnreachable));
    }

    #[test]
    fn decide_reports_cap_binding_when_no_probe_fits() {
        // Pre-compressed source: every probe over the cap, and none of it was "Met".
        let pts = vec![p(35.0, 9.6, 300.0), p(41.0, 9.5, 259.0)];
        let r = decide(&pts, &jod(9.5), 90.0, 14.0);
        assert_eq!(r.crf, 41.0);
        assert_eq!(r.size_pct, 259.0);
        assert!(matches!(r.outcome, SolveOutcome::CapBinding));
    }

    #[test]
    fn a_crf_that_holds_jod_but_bands_is_not_chosen() {
        let pts = vec![pc(30.0, 9.90, 0.4, 0.4), pc(36.0, 9.85, 2.5, 2.5)];
        let r = decide(&pts, &cambi(None, Some(1.0)), 90.0, 14.0);
        assert_eq!(r.crf, 30.0);
        assert_eq!(r.cambi, Some(Cambi { score: 0.4, diff: 0.4 }));
        assert!(matches!(r.outcome, SolveOutcome::Met));

        // Without the limit the same probes settle on the higher CRF.
        assert_eq!(decide(&pts, &jod(9.5), 90.0, 14.0).crf, 36.0);
    }

    #[test]
    fn the_total_and_the_diff_limit_are_checked_separately() {
        // A source that bands on its own: high total, nothing added by the encode.
        let banded_source = pc(30.0, 9.9, 6.0, 0.0);
        assert!(cambi(None, Some(1.0)).holds(&banded_source));
        assert!(!cambi(Some(5.0), None).holds(&banded_source));
        assert!(!cambi(Some(5.0), Some(1.0)).holds(&banded_source));

        // A clean source the encode bands: low total, all of it added.
        let added = pc(30.0, 9.9, 3.0, 3.0);
        assert!(cambi(Some(5.0), None).holds(&added));
        assert!(!cambi(None, Some(1.0)).holds(&added));
        assert!(cambi(Some(5.0), Some(3.0)).holds(&added));
    }

    #[test]
    fn cambi_is_measured_only_for_a_probe_that_holds_jod() {
        assert!(cambi(Some(5.0), None).needs_cambi(9.5));
        assert!(cambi(None, Some(1.0)).needs_cambi(9.8));
        assert!(!cambi(Some(5.0), Some(1.0)).needs_cambi(9.49));
        assert!(!jod(9.5).needs_cambi(9.8));
    }

    #[test]
    fn a_missing_cambi_reading_never_holds_a_cambi_floor() {
        assert!(!cambi(Some(5.0), None).holds(&p(30.0, 9.9, 50.0)));
        assert!(!cambi(None, Some(1.0)).holds(&p(30.0, 9.9, 50.0)));
        assert!(jod(9.5).holds(&p(30.0, 9.9, 50.0)));
    }

    #[test]
    fn banding_alone_bisects_instead_of_following_the_jod_secant() {
        let floor = cambi(Some(5.0), None);
        let next = |pts: &[Probe]| next_probe(pts, &floor, 90.0, 1.0, 70.0, false);

        // JOD is far above the floor, so its line would step one grid point at a time.
        let pts = [pc(20.0, 9.90, 4.5, 0.5), pc(40.0, 9.85, 7.0, 3.0)];
        assert_eq!(next(&pts), Some(30.0));
        let pts = [pc(20.0, 9.90, 4.5, 0.5), pc(30.0, 9.88, 4.8, 0.8), pc(40.0, 9.85, 7.0, 3.0)];
        assert_eq!(next(&pts), Some(35.0));
    }

    #[test]
    fn a_jod_failure_still_follows_the_line_with_a_cambi_floor_set() {
        // Both bracket ends are clean on CAMBI, so the JOD line places 35.
        let pts = vec![pc(30.0, 9.7, 0.2, 0.2), pc(40.0, 9.3, 0.6, 0.6)];
        assert_eq!(next_probe(&pts, &cambi(Some(5.0), Some(1.0)), 90.0, 1.0, 70.0, false), Some(35.0));
    }

    #[test]
    fn a_probe_over_the_cap_is_followed_up_where_its_size_is_estimated_to_fit() {
        // 120 % at 30 and 60 % at 40 cross 90 % a little above 34; the floor is out of reach either way.
        let pts = vec![p(30.0, 9.2, 120.0), p(40.0, 9.0, 60.0)];
        assert_eq!(next_probe(&pts, &jod(9.5), 90.0, 1.0, 70.0, false), Some(34.25));
        // Where the floor holds further up than the cap asks for, the floor places the probe.
        let pts = vec![p(30.0, 9.9, 120.0), p(40.0, 9.2, 60.0)];
        assert_eq!(next_probe(&pts, &jod(9.5), 90.0, 1.0, 70.0, false), Some(35.5));
        let pts = vec![p(34.0, 9.9, 91.0), p(34.25, 9.2, 89.0)];
        assert_eq!(next_probe(&pts, &jod(9.5), 90.0, 1.0, 70.0, false), None);
    }

    #[test]
    fn cambi_thresholds_follow_pq_only_for_a_pq_transfer() {
        let args = |t: &str| vec!["--transfer-characteristics".to_string(), t.to_string()];
        assert_eq!(cambi_feature(&args("16")), "cambi=full_ref=true:eotf=pq");
        assert_eq!(cambi_feature(&args("18")), "cambi=full_ref=true");
        assert_eq!(cambi_feature(&args("1")), "cambi=full_ref=true");
        assert_eq!(cambi_feature(&[]), "cambi=full_ref=true");
    }

    #[test]
    fn parse_cambi_reads_the_total_under_either_name_and_the_diff() {
        let raw = |total_key: &str| {
            let frames: Vec<String> = (0..20)
                .map(|i| format!(
                    r#"{{"frameNum":{i},"metrics":{{"{total_key}":{},"cambi_source":0.5,"cambi_full_reference":{}}}}}"#,
                    i as f64 * 0.5, i as f64 * 0.25,
                ))
                .collect();
            format!(r#"{{"version":"3f9e02a","fps":15.2,"frames":[{}],"pooled_metrics":{{}},"aggregate_metrics":{{}}}}"#,
                frames.join(","))
        };
        let expected = Cambi { score: 9.0, diff: 4.5 };
        assert_eq!(parse_cambi(&raw("cambi")).unwrap(), expected);
        // What libvmaf writes with eotf=pq, measured on v3.2.0.
        assert_eq!(parse_cambi(&raw("cambi_eotf_pq")).unwrap(), expected);

        assert!(parse_cambi(r#"{"frames":[]}"#).is_err());
        assert!(parse_cambi(r#"{"frames":[{"metrics":{"cambi":1.0}}]}"#).is_err());
        assert!(parse_cambi(r#"{"frames":[{"metrics":{"cambi_full_reference":1.0}}]}"#).is_err());
    }

    #[test]
    fn worst_frames_is_a_nearest_rank_percentile() {
        assert_eq!(worst_frames(&mut [3.0]), 3.0);
        assert_eq!(worst_frames(&mut (1..=100).map(f64::from).collect::<Vec<_>>()), 95.0);
        assert_eq!(worst_frames(&mut (1..=24).rev().map(f64::from).collect::<Vec<_>>()), 23.0);
    }

    fn tq(jod: f64) -> TargetQualityConfig {
        TargetQualityConfig { jod, ..Default::default() }
    }

    fn run_solve(
        cfg: &TargetQualityConfig,
        seed: f64,
        mut curve: impl FnMut(f64) -> Probe,
    ) -> (SolveResult, Vec<f64>) {
        let mut calls = Vec::new();
        let res = solve(cfg, seed, &mut |crf| {
            calls.push(crf);
            Ok(curve(crf))
        })
        .expect("a search over a working probe must not fail");
        (res, calls)
    }

    fn check_probes(cfg: &TargetQualityConfig, calls: &[f64]) {
        assert!(
            calls.len() <= cfg.max_probes as usize,
            "{} probes for a budget of {}: {calls:?}", calls.len(), cfg.max_probes
        );
        for crf in calls {
            assert!(
                *crf >= cfg.min_crf as f64 && *crf <= cfg.max_crf as f64,
                "probed crf {crf} outside {}..={}", cfg.min_crf, cfg.max_crf
            );
            assert!((crf / CRF_STEP).fract().abs() < 1e-9, "probed crf {crf} is off the grid");
        }
        for (i, a) in calls.iter().enumerate() {
            assert!(
                !calls[i + 1..].iter().any(|b| (a - b).abs() < 1e-9),
                "crf {a} probed twice, which costs a whole encode and measurement: {calls:?}"
            );
        }
    }

    #[test]
    fn the_search_settles_on_the_highest_crf_that_still_holds_the_floor() {
        // 9.5 JOD is held up to crf 26.0 exactly, and lost from 26.25 on.
        let cfg = tq(9.5);
        let (res, calls) = run_solve(&cfg, 35.5, |crf| p(crf, 10.0 - 0.02 * (crf - 1.0), 50.0));
        check_probes(&cfg, &calls);
        assert_eq!(res.crf, 26.0);
        assert!(matches!(res.outcome, SolveOutcome::Met));
    }

    #[test]
    fn min_probes_keeps_the_search_from_stopping_at_the_first_good_reading() {
        let flat = |crf: f64| p(crf, 9.52, 50.0);

        let (_, two) = run_solve(&tq(9.5), 30.0, flat);
        assert_eq!(two.len(), 2);

        let cfg = TargetQualityConfig { min_probes: 5, ..tq(9.5) };
        let (res, more) = run_solve(&cfg, 30.0, flat);
        check_probes(&cfg, &more);
        assert_eq!(more, [30.0, 30.75, 70.0]);
        assert!(matches!(res.outcome, SolveOutcome::Met));
    }

    #[test]
    fn a_black_chunk_goes_straight_to_max_crf() {
        let cfg = tq(9.5);
        let (res, calls) = run_solve(&cfg, 35.5, |crf| p(crf, 10.0, 5.0));
        assert_eq!(calls, [35.5, 55.5, 70.0]);
        assert_eq!(res.crf, 70.0);
    }

    #[test]
    fn a_binding_size_cap_does_not_leave_the_chunk_at_a_far_probe() {
        // A clip's measured curve: the second probe overshoots to max_crf, the rest creep in from below.
        let curve = [(1.0, 9.995), (20.0, 9.9427), (30.0, 9.9162), (40.0, 9.8553), (50.0, 9.7599), (60.0, 9.533), (70.0, 9.2)];
        let jod = |crf: f64| {
            let i = curve.windows(2).position(|w| crf <= w[1].0).unwrap_or(curve.len() - 2);
            let ((c0, j0), (c1, j1)) = (curve[i], curve[i + 1]);
            j0 + (crf - c0) / (c1 - c0) * (j1 - j0)
        };
        let cfg = TargetQualityConfig { max_encoded_percent: 50.0, ..tq(9.8) };
        let (res, calls) = run_solve(&cfg, 25.0, |crf| p(crf, jod(crf), 120.0 * (-0.1 * (crf - 35.5)).exp()));
        check_probes(&cfg, &calls);
        assert!(matches!(res.outcome, SolveOutcome::Met), "settled on crf {} via {calls:?}", res.crf);
        assert!(res.jod <= cfg.jod + cfg.tolerance, "settled on crf {} via {calls:?}", res.crf);
    }

    #[test]
    fn a_source_too_small_to_beat_sends_the_search_after_the_size_cap() {
        let cfg = tq(9.5);
        let (res, calls) = run_solve(&cfg, 20.0, |crf| p(crf, 9.9 - 0.01 * crf, 260.0 - 4.0 * crf));
        check_probes(&cfg, &calls);
        assert!(res.size_pct <= cfg.max_encoded_percent, "settled on {}% size", res.size_pct);
        assert!(matches!(res.outcome, SolveOutcome::CapBinding | SolveOutcome::Met));
    }

    #[test]
    fn a_floor_no_crf_reaches_spends_the_budget_and_stops() {
        let cfg = TargetQualityConfig { max_probes: 5, ..tq(9.9) };
        let (res, calls) = run_solve(&cfg, 35.0, |crf| p(crf, 9.0 - 0.01 * crf, 50.0));
        check_probes(&cfg, &calls);
        assert!(matches!(res.outcome, SolveOutcome::FloorUnreachable));
        assert_eq!(res.crf, calls.iter().copied().fold(f64::MAX, f64::min));
    }

    #[test]
    fn a_probe_that_fails_ends_the_search_instead_of_settling_on_a_guess() {
        let res = solve(&tq(9.5), 30.0, &mut |crf| {
            if crf == 30.0 { Ok(p(crf, 9.2, 50.0)) } else { bail!("FFVship failed") }
        });
        let Err(err) = res else { panic!("a failed measurement was swallowed") };
        assert!(err.to_string().contains("FFVship"));
    }

    #[test]
    fn a_probe_over_the_cap_ends_the_search_below_it() {
        let cfg = tq(9.85);
        let (res, calls) = run_solve(&cfg, 35.5, |crf| {
            p(crf, 10.0 - 6.074e-4 * crf.powf(1.802), 120.0 * (-0.07 * (crf - 35.5)).exp())
        });
        check_probes(&cfg, &calls);
        assert!(calls.iter().all(|&c| c >= 35.5), "probed below an over-cap crf: {calls:?}");
        assert!((39.75..=40.0).contains(&res.crf), "settled on crf {} via {calls:?}", res.crf);
        assert!(matches!(res.outcome, SolveOutcome::CapBinding));
    }

    #[test]
    fn the_budget_holds_on_a_curve_the_search_cannot_bracket() {
        // Non-monotonic: the interpolation aims at nothing and must still stop.
        let cfg = TargetQualityConfig { max_probes: 6, ..tq(9.5) };
        let (_, calls) = run_solve(&cfg, 35.0, |crf| p(crf, 9.5 + (crf * 7.0).sin() * 0.3, 80.0));
        check_probes(&cfg, &calls);
    }

    /// JOD by CRF of five 4K HDR clips, measured with svt-av1-hdr and joined by straight lines.
    const MEASURED: [(&str, [(f64, f64); 6]); 5] = [
        ("desert", [(1.0, 9.995), (20.0, 9.8594), (30.0, 9.7502), (40.0, 9.5082), (50.0, 9.2802), (60.0, 9.0419)]),
        ("interrogation", [(1.0, 9.995), (20.0, 9.9177), (30.0, 9.869), (40.0, 9.8173), (50.0, 9.7694), (60.0, 9.6111)]),
        ("space", [(1.0, 9.995), (20.0, 9.7862), (30.0, 9.7437), (40.0, 9.6834), (50.0, 9.6355), (60.0, 9.3644)]),
        ("village", [(1.0, 9.995), (20.0, 9.9427), (30.0, 9.9162), (40.0, 9.8553), (50.0, 9.7599), (60.0, 9.533)]),
        ("wreck", [(1.0, 9.995), (20.0, 9.9325), (30.0, 9.8755), (40.0, 9.8277), (50.0, 9.7378), (60.0, 9.4905)]),
    ];

    fn measured_jod(curve: &[(f64, f64)], crf: f64) -> f64 {
        let i = curve.windows(2).position(|w| crf <= w[1].0).unwrap_or(curve.len() - 2);
        let ((c0, j0), (c1, j1)) = (curve[i], curve[i + 1]);
        j0 + (crf - c0) / (c1 - c0) * (j1 - j0)
    }

    /// The answer a search with a probe at every CRF would give.
    fn oracle(cfg: &TargetQualityConfig, jod: impl Fn(f64) -> f64, size: impl Fn(f64) -> f64) -> f64 {
        let grid = (cfg.min_crf * 4..=cfg.max_crf * 4).map(|i| f64::from(i) * CRF_STEP);
        let fits: Vec<f64> = grid.filter(|&c| size(c) <= cfg.max_encoded_percent).collect();
        let both = fits.iter().copied().filter(|&c| jod(c) >= cfg.jod).reduce(f64::max);
        both.or(fits.first().copied()).unwrap_or(cfg.max_crf as f64)
    }

    #[test]
    fn the_measured_curves_are_solved_no_worse_than_before() {
        let (mut runs, mut near, mut probes, mut worst) = (0u32, 0u32, 0usize, 0f64);
        for (name, curve) in &MEASURED {
            let jod = |crf: f64| measured_jod(curve, crf);
            for (size_at_seed, slope) in [(0.0, 0.0), (60.0, 0.04), (90.0, 0.07), (120.0, 0.1), (180.0, 0.07), (300.0, 0.04)] {
                let size = |crf: f64| size_at_seed * (-slope * (crf - 35.5)).exp();
                for seed in [30.0, 35.5, 45.0] {
                    for target in (0..=12).map(|i| 9.3 + f64::from(i) * 0.05) {
                        let cfg = tq(target);
                        let (res, calls) = run_solve(&cfg, seed, |crf| p(crf, jod(crf), size(crf)));
                        check_probes(&cfg, &calls);
                        let gap = (res.crf - oracle(&cfg, jod, size)).abs();
                        assert!(gap <= 9.0, "{name} at {target:.2} from {seed}, size {size_at_seed}/{slope}: crf {} via {calls:?}", res.crf);
                        runs += 1;
                        near += u32::from(gap <= 0.5);
                        probes += calls.len();
                        worst = worst.max(gap);
                    }
                }
            }
        }
        // 79.2 % at 3.82 probes when this was written; a change may only move them the right way.
        let (near, probes) = (f64::from(near) / f64::from(runs), probes as f64 / f64::from(runs));
        assert!(near >= 0.79 && probes <= 3.85, "{:.1}% within half a CRF at {probes:.2} probes, worst {worst}", near * 100.0);
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn the_search_keeps_to_budget_range_and_grid_on_any_curve(
            (min_crf, max_crf) in (1u32..=40).prop_flat_map(|lo| (Just(lo), lo + 1..=70)),
            (min_probes, max_probes) in (2u32..=5).prop_flat_map(|lo| (Just(lo), lo..=lo + 5)),
            jod in 5.0f64..9.99,
            tolerance in 0.0f64..0.2,
            max_encoded_percent in 1.0f64..150.0,
            max_cambi in prop::option::weighted(0.3, 0.0f64..10.0),
            max_cambi_diff in prop::option::weighted(0.3, 0.0f64..5.0),
            seed in -10.0f64..80.0,
            (slope, noise, wave, decay, banding) in (0.0f64..0.1, 0.0f64..0.5, prop::option::weighted(0.2, 0.0f64..3.0), 0.02f64..0.1, 0.0f64..0.2),
            mut state in 1u64..u64::MAX,
        ) {
            let cfg = TargetQualityConfig {
                jod, min_crf, max_crf, min_probes, max_probes, tolerance, max_encoded_percent, max_cambi, max_cambi_diff,
                ..Default::default()
            };
            let floor = Floor::new(&cfg);
            let mut random = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64
            };
            // Readings scatter and need not fall with the CRF; the size does.
            let (res, calls) = run_solve(&cfg, seed, |crf| {
                let jod = 10.0 - slope * crf - (random() - 0.5) * noise + wave.map_or(0.0, |w| (crf * w).sin() * 0.4);
                let cambi = floor.needs_cambi(jod).then(|| Cambi { score: crf * banding + random(), diff: crf * banding / 2.0 + random() / 2.0 });
                Probe { crf, jod, cambi, size_pct: 400.0 * (-decay * crf).exp() }
            });
            check_probes(&cfg, &calls);
            prop_assert!(calls.iter().any(|crf| (crf - res.crf).abs() < 1e-9), "settled on an unprobed crf {}: {calls:?}", res.crf);

            let over_cap = |crf: f64| 400.0 * (-decay * crf).exp() > max_encoded_percent;
            for (i, crf) in calls.iter().enumerate() {
                prop_assert!(!calls[..i].iter().any(|earlier| over_cap(*earlier) && crf < earlier), "probed below an over-cap crf: {calls:?}");
            }
        }
    }

    #[test]
    fn measure_args_offsets_the_chunk_and_turns_the_crop_into_edge_amounts() {
        let opts = |crop| MeasureOpts {
            distorted: Path::new("probe.ivf"),
            source: Path::new("film.mkv"),
            index: Path::new("film.ffindex"),
            work_dir: Path::new("."),
            start: 720,
            frames: 48,
            crop,
            source_width: 1920,
            source_height: 1080,
            display_model: display_model_for(1920, 940, (1920, 940), &[]),
            gpu_id: 1,
            tag: "00003_28",
        };
        let pair = |args: &[String], flag: &str| {
            args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
        };

        // The probe holds the chunk's frames from 0; the source has them from 720.
        let plain = measure_args(&opts(None));
        assert_eq!(pair(&plain, "--start").as_deref(), Some("720"));
        assert_eq!(pair(&plain, "--encoded-offset").as_deref(), Some("-720"));
        assert_eq!(pair(&plain, "--displayModel").as_deref(), Some(DisplayModel::KEY));
        assert!(pair(&plain, "--displayConfig").is_some_and(|c| c.contains("[1920,940]")));
        assert_eq!(pair(&plain, "--gpu-id").as_deref(), Some("1"));
        assert!(plain.iter().any(|a| a == "--verbose"));
        assert!(!plain.iter().any(|a| a.starts_with("--crop")));

        let cropped = measure_args(&opts(Some(Crop { w: 1920, h: 800, x: 0, y: 140 })));
        assert_eq!(pair(&cropped, "--cropLeftSource").as_deref(), Some("0"));
        assert_eq!(pair(&cropped, "--cropTopSource").as_deref(), Some("140"));
        assert_eq!(pair(&cropped, "--cropRightSource").as_deref(), Some("0"));
        assert_eq!(pair(&cropped, "--cropBottomSource").as_deref(), Some("140"));

        let pillar = measure_args(&opts(Some(Crop { w: 1440, h: 1080, x: 240, y: 0 })));
        assert_eq!(pair(&pillar, "--cropLeftSource").as_deref(), Some("240"));
        assert_eq!(pair(&pillar, "--cropRightSource").as_deref(), Some("240"));
        assert_eq!(pair(&pillar, "--cropTopSource").as_deref(), Some("0"));
        assert_eq!(pair(&pillar, "--cropBottomSource").as_deref(), Some("0"));
    }

    #[test]
    fn decide_prefers_the_highest_crf_holding_both_constraints() {
        let pts = vec![p(20.0, 9.9, 95.0), p(30.0, 9.7, 80.0), p(36.0, 9.5, 70.0), p(44.0, 9.1, 50.0)];
        let r = decide(&pts, &jod(9.5), 90.0, 14.0);
        assert_eq!(r.crf, 36.0);
        assert!(matches!(r.outcome, SolveOutcome::Met));
    }
}
