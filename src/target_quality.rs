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

// Seeds the first interpolation only; measured on 4K HDR SVT-AV1 near the target zone.
const NOMINAL_JOD_PER_CRF: f64 = 0.025;

const CAMBI_PERCENTILE: f64 = 95.0;

/// The display CVVDP scores against. Vship takes pixels-per-degree from the model's own
/// resolution, never from the content, so the model has to carry the comparison's size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayModel {
    width: u32,
    height: u32,
    hdr: bool,
}

/// 30 inches at twice the display height, the geometry of Vship's own standard models.
const DISPLAY_INCHES: &str = "30";
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
             \"viewing_distance_meters\":{DISPLAY_DISTANCE_M},\"diagonal_size_inches\":{DISPLAY_INCHES},\
             \"max_luminance\":{max_luminance},\"contrast\":{contrast},\"E_ambient\":{ambient},\
             \"k_refl\":0.005}}}}",
            Self::KEY, self.width, self.height
        )
    }

    /// The name the config is looked up under; without it FFVship keeps its own default.
    pub const KEY: &'static str = "avet";

    pub fn describe(&self) -> String {
        let kind = if self.hdr { "HDR" } else { "SDR" };
        format!("{}x{} {kind}", self.width, self.height)
    }
}

/// HDR by the signalled transfer, at the resolution the two files are compared at.
pub fn display_model_for(width: u32, height: u32, hdr_args: &[String]) -> DisplayModel {
    let hdr = matches!(signalled_transfer(hdr_args), Some("16" | "18"));
    DisplayModel { width, height, hdr }
}

fn signalled_transfer(hdr_args: &[String]) -> Option<&str> {
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
    let text = String::from_utf8_lossy(&out.stdout);
    match select_gpu(&text) {
        Some(g) if g.hardware => Ok(g),
        Some(g) => bail!(
            "target_quality requires a GPU, but FFVship found only a software Vulkan device ({}). {HINT}",
            g.label
        ),
        None => bail!("target_quality requires a GPU, but FFVship found no Vulkan device. {HINT}"),
    }
}

/// First hardware device from `FFVship --list-gpu`, else the first software one.
fn select_gpu(list: &str) -> Option<GpuSelection> {
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
        .iter()
        .find(|d| d.hardware)
        .cloned()
        .or_else(|| devices.into_iter().next())
}

pub struct ProbeContext<'a> {
    pub source: &'a Path,
    pub index: &'a Path,
    pub temp_dir: &'a Path,
    pub config: &'a Config,
    pub opts: &'a EncodeOptions,
    pub tq: &'a TargetQualityConfig,
    pub display_model: DisplayModel,
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

#[derive(Clone, Copy)]
enum Phase {
    Search,
    ChasingCap,
    NarrowingCap,
}

impl Phase {
    fn suffix(self) -> &'static str {
        match self {
            Phase::Search       => "",
            Phase::ChasingCap   => " (chasing the size cap)",
            Phase::NarrowingCap => " (narrowing the size cap)",
        }
    }
}

/// Highest CRF holding `tq.jod` and the CAMBI limits under `tq.max_encoded_percent`. JOD
/// falls monotonically with CRF, so this is an interpolated binary search on the 0.25 grid.
pub fn solve_chunk_crf(ctx: &ProbeContext, scene: &SceneEntry) -> Result<SolveResult> {
    let lo = ctx.tq.min_crf as f64;
    let hi = ctx.tq.max_crf as f64;
    let key = scene.padded_index();
    let mut n = 0u32;

    solve(ctx.tq, seed_crf(ctx.config, lo, hi), &mut |crf, phase| {
        let probe = probe_once(ctx, scene, crf)?;
        n += 1;
        tracing::info!(
            "[{}] chunk {key} probe {n}/{} crf {crf} gives {}, {:.0}% size{}",
            ctx.stem, ctx.tq.max_probes, probe.scores(), probe.size_pct, phase.suffix()
        );
        Ok(probe)
    })
}

/// The search with the probe as a seam: driving it takes an encoder and a GPU, the
/// decisions it makes between probes take neither.
fn solve(
    tq: &TargetQualityConfig,
    seed: f64,
    probe_at: &mut dyn FnMut(f64, Phase) -> Result<Probe>,
) -> Result<SolveResult> {
    let lo = tq.min_crf as f64;
    let hi = tq.max_crf as f64;
    let floor = Floor::new(tq);
    let tol = tq.tolerance;
    let cap = tq.max_encoded_percent;

    let mut pts: Vec<Probe> = Vec::new();
    let mut crf = round_to_step(seed, lo, hi);

    for i in 0..tq.max_probes {
        let probe = probe_at(crf, Phase::Search)?;
        pts.push(probe);

        // early stop: just above the floor, within the size cap, after min_probes
        if i + 1 >= tq.min_probes
            && floor.holds(&probe) && probe.jod <= floor.jod + tol && probe.size_pct <= cap
        {
            break;
        }
        // Size falls as the CRF rises, so nothing below a probe over the cap can be picked.
        let pickable = pts.iter().filter(|p| p.size_pct > cap).map(|p| p.crf + CRF_STEP).fold(lo, f64::max);
        match next_crf(&pts, &floor, lo, hi) {
            Some(next) if (next - crf).abs() > 1e-9 && next >= pickable - 1e-9 => crf = next,
            _ => break,
        }
    }

    // The search above follows the floor only, so every probe can be over the cap.
    while (pts.len() as u32) < tq.max_probes && !pts.iter().any(|p| p.size_pct <= cap) {
        let highest = pts.iter().map(|p| p.crf).fold(f64::MIN, f64::max);
        if highest >= hi - 1e-9 {
            break;
        }
        let next = round_to_step((highest + hi) / 2.0, highest + CRF_STEP, hi);
        if already(&pts, next) {
            break;
        }
        let probe = probe_at(next, Phase::ChasingCap)?;
        pts.push(probe);
    }

    // That bisection can overshoot, and a lower CRF under the cap is free quality.
    while (pts.len() as u32) < tq.max_probes
        && !pts.iter().any(|p| floor.holds(p) && p.size_pct <= cap)
    {
        let Some(fit) = pts.iter().filter(|p| p.size_pct <= cap).map(|p| p.crf).reduce(f64::min)
        else {
            break;
        };
        let Some(over) = pts
            .iter()
            .filter(|p| p.size_pct > cap && p.crf < fit)
            .map(|p| p.crf)
            .reduce(f64::max)
        else {
            break;
        };
        // Grid-aligned, so anything above one step is at least two.
        if fit - over <= CRF_STEP + 1e-9 {
            break;
        }
        let next = round_to_step((over + fit) / 2.0, over + CRF_STEP, fit - CRF_STEP);
        if already(&pts, next) {
            break;
        }
        let probe = probe_at(next, Phase::NarrowingCap)?;
        pts.push(probe);
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
    let opts = EncodeOptions { dynamic_hdr: Default::default(), hdr10plus_frames: None, ..ctx.opts.clone() };
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

    let gpu = ctx.gpu_lock.lock().unwrap();
    let jod = measure(&MeasureOpts {
        distorted: &probe,
        source: ctx.source,
        index: ctx.index,
        work_dir: ctx.temp_dir,
        start: scene.start_frame,
        crop: ctx.opts.crop,
        source_width: ctx.source_width,
        source_height: ctx.source_height,
        display_model: ctx.display_model,
        gpu_id: ctx.gpu_id,
        n_threads: ctx.n_threads,
        tag: &tag,
    });
    drop(gpu);
    let result = jod.and_then(|jod| {
        let cambi = Floor::new(ctx.tq).needs_cambi(jod)
            .then(|| measure_cambi(ctx, scene, &probe, &tag))
            .transpose()?;
        Ok((jod, cambi))
    });
    let _ = std::fs::remove_file(&probe);
    let (jod, cambi) = result?;
    let size_pct = chunk_size_pct(size_bytes, ctx.source_byte_index, scene.start_frame, scene.end_frame);
    Ok(Probe { crf, jod, cambi, size_pct })
}

/// Percent of the source's own bytes for these frames; 0 when the index is missing.
pub fn chunk_size_pct(encoded: u64, cum: &[u64], start: u64, end: u64) -> f64 {
    match (cum.get(start as usize), cum.get(end as usize + 1)) {
        (Some(lo), Some(hi)) if hi > lo => encoded as f64 / (hi - lo) as f64 * 100.0,
        _ => 0.0,
    }
}

/// None once the crossing is bracketed to one step, a bound is hit, or the grid is used.
fn next_crf(pts: &[Probe], floor: &Floor, lo: f64, hi: f64) -> Option<f64> {
    let target = floor.jod;
    let pass: Vec<f64> = pts.iter().filter(|p| floor.holds(p)).map(|p| p.crf).collect();
    let fail: Vec<f64> = pts.iter().filter(|p| !floor.holds(p)).map(|p| p.crf).collect();
    // A probe that held JOD and failed on banding gives the JOD secant nothing to aim at.
    let by_jod = pts.iter().filter(|p| !floor.holds(p)).all(|p| p.jod < target);

    if !pass.is_empty() && !fail.is_empty() {
        let p = pass.iter().copied().fold(f64::MIN, f64::max); // highest CRF still passing
        let f = fail.iter().copied().fold(f64::MAX, f64::min); // lowest CRF failing
        if f - p <= CRF_STEP + 1e-9 {
            return None; // bracketed to adjacent grid steps
        }
        let guess = if by_jod { interpolate_crf(pts, target) } else { (p + f) / 2.0 };
        let mut cand = round_to_step(guess, p, f);
        if cand <= p + 1e-9 || cand >= f - 1e-9 || already(pts, cand) {
            cand = round_to_step((p + f) / 2.0, p, f); // bisection fallback
        }
        if cand <= p + 1e-9 || cand >= f - 1e-9 || already(pts, cand) {
            return None;
        }
        Some(cand)
    } else if !pass.is_empty() {
        // everything passes: compress harder (toward max_crf)
        let hp = pass.iter().copied().fold(f64::MIN, f64::max);
        if hp >= hi - 1e-9 {
            return None;
        }
        let cand = round_to_step(interpolate_crf(pts, target).max(hp + CRF_STEP), hp + CRF_STEP, hi);
        if already(pts, cand) { None } else { Some(cand) }
    } else {
        // everything fails: raise quality (toward min_crf)
        let lf = fail.iter().copied().fold(f64::MAX, f64::min);
        if lf <= lo + 1e-9 {
            return None;
        }
        let guess = if by_jod { interpolate_crf(pts, target).min(lf - CRF_STEP) } else { (lo + lf) / 2.0 };
        let cand = round_to_step(guess, lo, lf - CRF_STEP);
        if already(pts, cand) { None } else { Some(cand) }
    }
}

/// Linear (secant) estimate of the CRF that yields `target` JOD.
fn interpolate_crf(pts: &[Probe], target: f64) -> f64 {
    if pts.len() == 1 {
        return pts[0].crf + (pts[0].jod - target) / NOMINAL_JOD_PER_CRF;
    }
    let (a, b) = bracket_pts(pts, target);
    if (a.1 - b.1).abs() < 1e-6 {
        return (a.0 + b.0) / 2.0;
    }
    let slope = (b.0 - a.0) / (b.1 - a.1);
    a.0 + slope * (target - a.1)
}

/// One point at/above and one below the target, else the two closest in JOD.
fn bracket_pts(pts: &[Probe], target: f64) -> ((f64, f64), (f64, f64)) {
    let above = pts.iter().filter(|p| p.jod >= target).min_by(|x, y| x.jod.total_cmp(&y.jod));
    let below = pts.iter().filter(|p| p.jod <  target).max_by(|x, y| x.jod.total_cmp(&y.jod));
    if let (Some(a), Some(b)) = (above, below) {
        return ((a.crf, a.jod), (b.crf, b.jod));
    }
    let mut sorted: Vec<&Probe> = pts.iter().collect();
    sorted.sort_by(|x, y| (x.jod - target).abs().total_cmp(&(y.jod - target).abs()));
    ((sorted[0].crf, sorted[0].jod), (sorted[1].crf, sorted[1].jod))
}

fn round_to_step(v: f64, lo: f64, hi: f64) -> f64 {
    ((v / CRF_STEP).round() * CRF_STEP).clamp(lo, hi)
}

fn already(pts: &[Probe], crf: f64) -> bool {
    pts.iter().any(|p| (p.crf - crf).abs() < 1e-9)
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
    crop: Option<Crop>,
    source_width: u32,
    source_height: u32,
    display_model: DisplayModel,
    gpu_id: u32,
    n_threads: usize,
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
    const RECOVERABLE: &[&str] = &[
        "VK_ERROR_DEVICE_LOST",
        "VK_ERROR_OUT_OF_DEVICE_MEMORY",
        "VK_ERROR_INITIALIZATION_FAILED",
        "out of device memory",
        "no Vulkan device",
        "OutOfVRAM",
    ];
    let err = crate::ext::tool_error(what, status, stderr);
    if err.downcast_ref::<crate::job::Transient>().is_none()
        && RECOVERABLE.iter().any(|m| stderr.contains(m))
    {
        return err.context(crate::job::Transient);
    }
    err
}

/// FFVship crops the source to match and resizes on a mismatch; its last cumulative
/// JOD is the chunk score.
fn measure(m: &MeasureOpts) -> Result<f64> {
    let json = m.work_dir.join(format!("cvvdp_{}.json", m.tag));
    let _cleanup = Cleanup(vec![json.clone()]);

    let mut cmd = std::process::Command::new(external_bin("FFVship"));
    cmd.arg("-s").arg(m.source)
        .arg("-e").arg(m.distorted)
        .arg("--source-index").arg(m.index)
        .args(measure_args(m))
        .arg("--json").arg(&json);

    // A wedged GPU takes the worker with it, and nothing above would notice.
    const TIMEOUT_SECS: u64 = 1800;

    let out = crate::ext::output_with_timeout(&mut cmd, TIMEOUT_SECS, "FFVship")?;
    if !out.status.success() {
        return Err(gpu_error("FFVship", out.status, &String::from_utf8_lossy(&out.stderr)));
    }

    let raw = std::fs::read_to_string(&json)
        .with_context(|| format!("read FFVship json: {}", json.display()))?;
    parse_cvvdp(&raw)
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
        // FFVship's own help calls this the number of decoder processes and recommends 2;
        // it is not the encoder's thread count, and each one holds decoded frames.
        "-t".into(), m.n_threads.clamp(1, 4).to_string(),
        "-g".into(), "3".into(),
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
    const TIMEOUT_SECS: u64 = 1800;

    let fifo = ctx.temp_dir.join(format!("cambi_{tag}.y4m"));
    let json = ctx.temp_dir.join(format!("cambi_{tag}.json"));
    let _ = std::fs::remove_file(&fifo);
    let _cleanup = Cleanup(vec![fifo.clone(), json.clone()]);
    make_fifo(&fifo)?;
    // O_RDWR on a Linux FIFO never blocks, so neither child hangs opening its end, and
    // dropping it is what lets vmaf see EOF, or the decoder EPIPE, once the other is gone.
    let hold = std::fs::OpenOptions::new().read(true).write(true).open(&fifo)
        .with_context(|| format!("open {}", fifo.display()))?;

    let mut vmaf = std::process::Command::new(external_bin("vmaf"))
        .args(["--reference", "/dev/stdin", "--distorted"]).arg(&fifo)
        .args(["--no_prediction", "--threads", &ctx.n_threads.to_string()])
        .args(["--feature", cambi_feature(&ctx.opts.hdr_args), "--json", "--output"]).arg(&json)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("start vmaf")?;
    let mut decoder = match std::process::Command::new(external_bin("ffmpeg"))
        // CAMBI only scores flat areas, and synthesized grain leaves none: it would read 0.
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-export_side_data", "film_grain", "-i"]).arg(probe)
        .args(["-map", "0:v:0", "-strict", "-1", "-f", "yuv4mpegpipe"]).arg(&fifo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            reap(&mut vmaf);
            return Err(e).context("start ffmpeg probe decoder");
        }
    };

    let sink = vmaf.stdin.take().expect("reference input is piped");
    let (source, index, opts) = (ctx.source.to_path_buf(), ctx.index.to_path_buf(), ctx.opts.clone());
    let (start, end) = (scene.start_frame, scene.end_frame);
    let writer = std::thread::spawn(move || -> Result<()> {
        let mut vs = VideoSource::open(&source, &index, OpenOpts { target_bit_depth: opts.target_bit_depth })
            .context("open FFMS2 VideoSource")?;
        vs.info.fps_num = opts.fps_num;
        vs.info.fps_den = opts.fps_den;
        let mut out = std::io::BufWriter::with_capacity(256 * 1024, sink);
        vs.write_y4m_range(&mut out, start, end, opts.crop, None)
    });

    let vmaf_err = crate::ext::drain_text(vmaf.stderr.take());
    let dec_err = crate::ext::drain_text(decoder.stderr.take());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(TIMEOUT_SECS);
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
                    .context(format!("vmaf did not finish within {TIMEOUT_SECS}s - killed")));
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
        let mut err = anyhow!("{}", failed.iter().map(|e| e.root_cause().to_string()).collect::<Vec<_>>().join("\n"));
        if let Some(cause) = encode::feed_failure(write_res) {
            err = err.context(format!("reading the source failed first: {cause}"));
        }
        return Err(if transient { err.context(crate::job::Transient) } else { err });
    }
    write_res.context("write Y4M reference to vmaf")?;

    let raw = std::fs::read_to_string(&json)
        .with_context(|| format!("read vmaf json: {}", json.display()))?;
    parse_cambi(&raw)
}

/// CAMBI's visibility thresholds come in BT.1886 and PQ; HLG is measured as BT.1886.
fn cambi_feature(hdr_args: &[String]) -> &'static str {
    match signalled_transfer(hdr_args) {
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
    fn display_model_follows_the_signalled_transfer() {
        let args = |t: &str| vec!["--transfer-characteristics".to_string(), t.to_string()];

        assert_eq!(display_model_for(3840, 2160, &args("16")).describe(), "3840x2160 HDR");
        assert_eq!(display_model_for(3840, 2160, &args("18")).describe(), "3840x2160 HDR");
        assert_eq!(display_model_for(1280, 720, &args("18")).describe(), "1280x720 HDR");

        // An SDR source signals bt709; an HDR display costs it ~2.5 JOD.
        assert_eq!(display_model_for(1920, 1080, &args("1")).describe(), "1920x1080 SDR");
        assert_eq!(display_model_for(3840, 2160, &[]).describe(), "3840x2160 SDR");
    }

    #[test]
    fn the_display_config_carries_the_comparison_resolution() {
        let pq = vec!["--transfer-characteristics".to_string(), "16".to_string()];

        // Vship reads pixels-per-degree off the model, so this is what fixes the reading.
        let hdr = display_model_for(1920, 1080, &pq).config_json();
        assert!(hdr.contains("\"resolution\":[1920,1080]"), "{hdr}");
        assert!(hdr.contains("\"colorspace\":\"HDR\""), "{hdr}");
        assert!(hdr.contains("\"max_luminance\":1500"), "{hdr}");
        assert!(hdr.starts_with(&format!("{{\"{}\":", DisplayModel::KEY)), "{hdr}");

        let sdr = display_model_for(3840, 2160, &[]).config_json();
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
    fn select_gpu_prefers_hardware() {
        let list = "GPU 0: NVIDIA GeForce RTX 5060 Ti\nGPU 1: llvmpipe (LLVM 22.1.7, 256 bits)\n";
        let g = select_gpu(list).unwrap();
        assert_eq!(g.id, 0);
        assert!(g.hardware);
    }

    #[test]
    fn select_gpu_picks_hardware_even_after_software() {
        let list = "GPU 0: llvmpipe (LLVM 22.1.7)\nGPU 1: Intel Graphics\n";
        let g = select_gpu(list).unwrap();
        assert_eq!(g.id, 1);
        assert!(g.hardware);
    }

    #[test]
    fn select_gpu_reports_software_only() {
        let g = select_gpu("GPU 0: llvmpipe (LLVM 22.1.7)\n").unwrap();
        assert_eq!(g.id, 0);
        assert!(!g.hardware);
    }

    #[test]
    fn parse_cvvdp_takes_last_cumulative() {
        assert!((parse_cvvdp("[[9.95],[9.90],[9.83]]").unwrap() - 9.83).abs() < 1e-9);
        assert!(parse_cvvdp("[]").is_err());
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
    fn interpolate_hits_crossing() {
        let pts = vec![p(30.0, 9.7, 0.0), p(40.0, 9.3, 0.0)];
        assert!((interpolate_crf(&pts, 9.5) - 35.0).abs() < 1e-6);
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

        // JOD is far above the floor, so its secant would step one grid point at a time.
        assert_eq!(next_crf(&[pc(35.0, 9.88, 8.0, 8.0)], &floor, 1.0, 70.0), Some(18.0));

        let pts = [pc(20.0, 9.90, 4.5, 0.5), pc(40.0, 9.85, 7.0, 3.0)];
        assert_eq!(next_crf(&pts, &floor, 1.0, 70.0), Some(30.0));
    }

    #[test]
    fn a_jod_failure_still_interpolates_with_a_cambi_floor_set() {
        // Both bracket ends are clean on CAMBI, so the JOD secant places 35.
        let pts = vec![pc(30.0, 9.7, 0.2, 0.2), pc(40.0, 9.3, 0.6, 0.6)];
        assert_eq!(next_crf(&pts, &cambi(Some(5.0), Some(1.0)), 1.0, 70.0), Some(35.0));
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
        let res = solve(cfg, seed, &mut |crf, _| {
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
        let (res, five) = run_solve(&cfg, 30.0, flat);
        check_probes(&cfg, &five);
        assert_eq!(five.len(), 5);
        assert!(matches!(res.outcome, SolveOutcome::Met));
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
        let res = solve(&tq(9.5), 30.0, &mut |crf, _| {
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

    #[test]
    fn measure_args_offsets_the_chunk_and_turns_the_crop_into_edge_amounts() {
        let opts = |crop| MeasureOpts {
            distorted: Path::new("probe.ivf"),
            source: Path::new("film.mkv"),
            index: Path::new("film.ffindex"),
            work_dir: Path::new("."),
            start: 720,
            crop,
            source_width: 1920,
            source_height: 1080,
            display_model: display_model_for(1920, 940, &[]),
            gpu_id: 1,
            n_threads: 6,
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
