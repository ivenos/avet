use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::audio;
use crate::config::{Config, TargetQualityConfig, VideoMode};
use crate::encode::{self, EncodeOptions};
use crate::ffms2::{self, Crop};
use crate::resume::{CrfCache, DoneFile, SceneEntry, TempDir};
use crate::scanner::Job;
use crate::scene;
use crate::target_quality;
use crate::workers;

pub struct JobContext {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
}

/// Clears on its own or on the user's next edit: retried, never marked `.failed`.
#[derive(Debug)]
pub struct Transient;

impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retrying on the next scan")
    }
}

impl std::error::Error for Transient {}

/// `downcast_ref`, not `chain()`: a `.context()` value is not a link there. ENOSPC too.
fn is_transient(err: &anyhow::Error) -> bool {
    if err.downcast_ref::<Transient>().is_some() {
        return true;
    }
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull)
    })
}

/// Per-job context, shared by every chunk worker.
struct WorkerCtx<'a> {
    source: &'a Path,
    temp: &'a TempDir,
    config: &'a Config,
    opts: &'a EncodeOptions,
    tq_display_model: Option<&'static str>,
    tq_gpu_id: Option<u32>,
    gpu_lock: Mutex<()>,
    source_width: u32,
    source_height: u32,
    crf_cache: Option<CrfCache>,
    threads_per_worker: usize,
    stem: &'a str,
    total_chunks: usize,
    total_frames: u64,
    /// Cumulative source byte sizes by frame for the size cap; empty when unused.
    source_byte_index: Vec<u64>,
    done: DoneFile,
    completed_chunks: AtomicUsize,
    completed_frames: AtomicU64,
    /// Set when a chunk fails, so queued workers return instead of starting.
    cancel: AtomicBool,
}

pub fn run(job: &Job, ctx: &JobContext) -> Result<()> {
    // Marking every video in the folder would keep them all skipped after the fix.
    let config = Config::from_file(&job.encode_toml).context(Transient)?;

    let stem = job.stem();

    wait_for_stable(&job.source_file, stem)?;

    let temp = TempDir::for_video(&job.output_dir(&ctx.output_dir), stem);
    temp.claim_source(&job.source_file, stem)?;

    if config.avet.video == VideoMode::Copy {
        return run_copy(job, ctx, &config, stem, &temp);
    }

    if !temp.index_path.exists() {
        tracing::info!("[{stem}] indexing");
        ffms2::run_ffmsindex(&job.source_file, &temp.index_path)?;
        tracing::info!("[{stem}] indexing done");
    } else {
        tracing::info!("[{stem}] reusing existing index");
    }

    let video_source = ffms2::VideoSource::open(&job.source_file, &temp.index_path, ffms2::OpenOpts::default())?;
    let video_info = video_source.info.clone();
    let source_timestamps = video_source.timestamps_ms()?;
    drop(video_source);

    let threads_per_worker = config.encoder_params
        .get("lp")
        .and_then(|v| match v {
            toml::Value::Integer(i) => usize::try_from(*i).ok(),
            toml::Value::String(s)  => s.parse().ok(),
            _ => None,
        })
        .filter(|&n| n > 0)
        .unwrap_or(6);
    let num_workers = workers::calculate(&video_info, stem, threads_per_worker);

    let (fps_num, fps_den) = probe_fps(&job.source_file)?;
    let fps = fps_num as f64 / fps_den as f64;

    let timestamps = match vfr_timestamps(&source_timestamps, fps_num, fps_den, stem) {
        Some(ts) => {
            tracing::info!("[{stem}] variable frame rate: keeping the source timestamps");
            write_timestamps(&temp.timestamps_path, &ts)?;
            Some(temp.timestamps_path.as_path())
        }
        None => None,
    };

    let hdr_args: Vec<String> = if config.avet.hdr {
        let hdr = crate::hdr::detect(&job.source_file)?;
        // Profile 5's base layer is IPT-PQ-C2, an image only once the RPU is applied.
        if hdr.dv_profile == Some(5) {
            bail!(
                "Dolby Vision profile 5 has no HDR10 base layer, so avet cannot encode it \
                 correctly. Convert the source to profile 8 or to plain HDR10 first."
            );
        }
        match (hdr.dv_profile, hdr.hdr_type.as_str()) {
            (Some(p), _) => tracing::info!(
                "[{stem}] HDR: Dolby Vision profile {p} (RPU dropped, HDR10 base layer kept)"
            ),
            (None, "Dolby Vision") => tracing::warn!(
                "[{stem}] HDR: Dolby Vision of unknown profile (RPU dropped; the base layer \
                 is only a valid HDR10 picture on profiles 7 and 8)"
            ),
            (None, "HDR10+") => tracing::info!(
                "[{stem}] HDR: HDR10+ (dynamic metadata dropped, static HDR10 kept)"
            ),
            (None, t) => tracing::info!("[{stem}] HDR: {t}"),
        }
        hdr.encoder_args()
    } else {
        Vec::new()
    };

    let crop_str: Option<String> = if config.avet.crop {
        let duration_secs = video_info.num_frames as f64 / fps;
        crate::crop::detect(&job.source_file, duration_secs, &temp.crop_cache, stem)?
    } else {
        None
    };

    let (scale_target, crop, scene_vf) = compute_output_params(
        video_info.width,
        video_info.height,
        crop_str.as_deref(),
        config.avet.scale,
        stem,
    );

    let auto_keyint: Option<u32> = if config.avet.keyint {
        let ki = (fps * 5.0).round().max(1.0) as u32;
        tracing::info!("[{stem}] auto-keyint: {ki} ({fps:.3} fps, keyframe every ~5s)");
        Some(ki)
    } else {
        None
    };

    let encode_opts = EncodeOptions {
        hdr_args,
        keyint: auto_keyint,
        scale: scale_target,
        crop,
        fps_num,
        fps_den,
        target_bit_depth: config.avet.bit_depth,
    };

    let merged_args = encode::merged_encoder_args(&config, &encode_opts);

    // So a resumed encode never mixes chunks from different settings.
    let fingerprint = profile_fingerprint(
        config.encoder, &merged_args, &encode_opts, &config.scene_detection,
        config.target_quality.as_ref(),
    );
    invalidate_stale_cache(&temp, &fingerprint, stem)?;

    // Non-empty, not just present: the fingerprint keeps a crash leftover alive forever.
    let have_scenes = std::fs::metadata(&temp.scenes_path).is_ok_and(|m| m.len() > 0);
    let scenes: Vec<SceneEntry> = if have_scenes {
        tracing::info!("[{stem}] reusing scenes.json");
        crate::resume::read_scenes(&temp.scenes_path)?
    } else {
        tracing::info!("[{stem}] scene detection");
        let scenes = scene::detect(
            &job.source_file,
            &config.scene_detection,
            scene_vf.as_deref(),
            fps,
        )?;
        crate::resume::write_scenes(&temp.scenes_path, &scenes)?;
        tracing::info!("[{stem}] {} chunks", scenes.len());
        scenes
    };

    // clamp to FFMS2 frame count - scene detector may overcount on broken remuxes
    let ffms2_frames = video_info.num_frames as u64;
    let mut scenes: Vec<SceneEntry> = scenes
        .into_iter()
        .filter_map(|mut s| {
            if s.start_frame >= ffms2_frames {
                tracing::warn!(
                    "[{stem}] dropping scene {} (start {} >= FFMS2 frame count {})",
                    s.index, s.start_frame, ffms2_frames
                );
                return None;
            }
            if s.end_frame >= ffms2_frames {
                tracing::warn!(
                    "[{stem}] clamping scene {} end_frame {} to {}",
                    s.index, s.end_frame, ffms2_frames - 1
                );
                s.end_frame = ffms2_frames - 1;
            }
            Some(s)
        })
        .collect();

    // The detector counts decoded frames, FFMS2 counts them in the container.
    match scenes.last_mut() {
        None => bail!("scene detection produced no chunks within the source frame count"),
        Some(last) if last.end_frame + 1 < ffms2_frames => {
            tracing::warn!(
                "[{stem}] scenes cover {} of {ffms2_frames} frames - extending the last chunk to the end",
                last.end_frame + 1
            );
            last.end_frame = ffms2_frames - 1;
        }
        Some(_) => {}
    }

    let total_chunks = scenes.len();
    let total_frames: u64 = scenes.iter().map(|s| s.frame_count()).sum();

    let summary: Vec<String> = merged_args
        .chunks(2)
        .filter_map(|pair| match pair {
            [k, v] => Some(format!("{}={}", k.trim_start_matches('-'), v)),
            _      => None,
        })
        .collect();
    tracing::info!("[{stem}] encoder args: {}", summary.join(" "));

    let audio_plan = audio::plan(&job.source_file, &config.audio)?;
    for line in audio_plan.summary_lines() {
        tracing::info!("[{stem}] audio {line}");
    }

    // FFVship compares at source resolution, so the model follows the crop, not the scale.
    let reference_height = encode_opts.crop.map(|c| c.h).unwrap_or(video_info.height);
    let (tq_display_model, tq_gpu_id, crf_cache): (Option<&'static str>, Option<u32>, Option<CrfCache>) =
        if let Some(tq) = &config.target_quality {
            // A driver upgrade or a GPU in reset clears on its own.
            let gpu = target_quality::ensure_available().context(Transient)?;
            let display_model =
                target_quality::display_model_for(reference_height, &encode_opts.hdr_args);
            tracing::info!(
                "[{stem}] target quality: JOD {} floor (display {display_model}, {}, crf {}-{}, {}-{} probes, probe preset {}, max {}% size)",
                tq.jod, gpu.describe(), tq.min_crf, tq.max_crf, tq.min_probes, tq.max_probes, tq.probe_preset, tq.max_encoded_percent
            );
            if let Some(c) = tq.max_cambi {
                tracing::info!("[{stem}] target quality: CAMBI at most {c}");
            }
            if let Some(c) = tq.max_cambi_diff {
                tracing::info!("[{stem}] target quality: CAMBI at most {c} above the encoder input");
            }
            if config.encoder_params.contains_key("crf") {
                tracing::info!("[{stem}] target quality: crf in encoder_params used only as a probe seed");
            }
            (Some(display_model), Some(gpu.id), Some(CrfCache::load_or_create(&temp.tq_path)?))
        } else {
            (None, None, None)
        };

    tracing::info!("[{stem}] encoding: {total_chunks} chunks, {num_workers} worker(s)");

    let source_byte_index = if config.target_quality.is_some() {
        probe_source_byte_index(&job.source_file, stem)
    } else {
        Vec::new()
    };

    let wctx = WorkerCtx {
        source: &job.source_file,
        temp: &temp,
        config: &config,
        opts: &encode_opts,
        tq_display_model,
        tq_gpu_id,
        gpu_lock: Mutex::new(()),
        source_width: video_info.width,
        source_height: video_info.height,
        crf_cache,
        threads_per_worker,
        stem,
        total_chunks,
        total_frames,
        source_byte_index,
        done: DoneFile::load_or_create(&temp.done_path)?,
        completed_chunks: AtomicUsize::new(0),
        completed_frames: AtomicU64::new(0),
        cancel: AtomicBool::new(false),
    };

    let mut pending = Vec::new();
    for scene in &scenes {
        let chunk_key = scene.padded_index();
        if wctx.done.is_done(&chunk_key, &temp.chunk_path(&chunk_key)) {
            wctx.completed_chunks.fetch_add(1, Ordering::Relaxed);
            wctx.completed_frames.fetch_add(scene.frame_count(), Ordering::Relaxed);
            tracing::debug!("[{stem}] chunk {chunk_key} already done");
        } else {
            pending.push(scene);
        }
    }

    let queue = Mutex::new(pending.into_iter());
    let first_err = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..num_workers {
            s.spawn(|| loop {
                if wctx.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let Some(scene) = queue.lock().unwrap().next() else { break };
                if let Err(e) = encode_one(&wctx, scene) {
                    wctx.cancel.store(true, Ordering::Relaxed);
                    first_err.lock().unwrap().get_or_insert(e);
                }
            });
        }
    });
    if let Some(e) = first_err.into_inner().unwrap() {
        return Err(e);
    }

    tracing::info!("[{stem}] merging chunks");
    // Inside the temp dir, so a failure here leaves nothing stranded next to the output.
    let video_only  = temp.video_path.clone();
    let chunk_paths: Vec<PathBuf> =
        scenes.iter().map(|s| temp.chunk_path(&s.padded_index())).collect();
    encode::concat_chunks(&chunk_paths, &video_only, &temp.path)?;

    tracing::info!("[{stem}] processing audio");
    let video = MuxVideo { path: &video_only, timestamps, remove: true, expected_frames: Some(total_frames) };
    finalize(job, ctx, &config, &temp, &audio_plan, video)
}

struct MuxVideo<'a> {
    /// The merged encode (run) or the untouched source (run_copy).
    path: &'a Path,
    /// Timecodes v2 file replacing the encode's constant frame rate.
    timestamps: Option<&'a Path>,
    /// Delete `path` after muxing; true only when it is avet's own temp file.
    remove: bool,
    /// Frames the finished file has to hold; None for `video = copy`.
    expected_frames: Option<u64>,
}

/// Shared tail of run/run_copy: process audio, mux, validate, archive source, clean up.
fn finalize(
    job: &Job,
    ctx: &JobContext,
    config: &Config,
    temp: &TempDir,
    audio_plan: &audio::AudioPlan,
    video: MuxVideo<'_>,
) -> Result<()> {
    let stem = job.stem();
    audio::process_plan(&job.source_file, &temp.audio_path, audio_plan)?;
    let audio_path = &temp.audio_path;

    let subtitle_sel = crate::subtitle::select_tracks(&job.source_file, &config.subtitles)?;

    let final_output = job.output_dir(&ctx.output_dir).join(format!("{stem}.mkv"));
    // An empty file is a leftover, not a result; the scanner ignores it for the same reason.
    match std::fs::metadata(&final_output) {
        Ok(m) if m.len() > 0 => bail!("output already exists: {}", final_output.display()),
        Ok(_) => tracing::warn!("[{stem}] replacing empty {}", final_output.display()),
        Err(_) => {}
    }

    // Into the temp dir first: the next scan reads a half-written output as "already done".
    tracing::info!("[{stem}] muxing to {}", final_output.display());
    audio::mux_final(video.path, video.timestamps, audio_path, &job.source_file, &temp.mux_path, &subtitle_sel)?;

    if video.remove {
        let _ = std::fs::remove_file(video.path);
    }

    tracing::info!("[{stem}] validating output");
    encode::validate_output(&temp.mux_path, video.expected_frames)?;

    std::fs::rename(&temp.mux_path, &final_output).with_context(|| {
        format!("move {} to {}", temp.mux_path.display(), final_output.display())
    })?;

    // Delivered: the scanner short-circuits on "output exists", so nothing below retries.
    if let Err(e) = archive_source(job, ctx) {
        tracing::error!("[{stem}] output is in place, but archiving the source failed: {e:#}");
    }

    if !config.avet.keep_temp
        && let Err(e) = std::fs::remove_dir_all(&temp.path)
    {
        tracing::error!("[{stem}] could not remove temp dir {}: {e:#}", temp.path.display());
    }

    tracing::info!("[{stem}] done");
    Ok(())
}

/// Never over an existing file: two seasons can each have an `Episode 01.mkv`.
fn archive_source(job: &Job, ctx: &JobContext) -> Result<()> {
    let processed_dir = crate::scanner::ensure_processed_dir(&ctx.input_dir, &job.rel_dir)?;
    let name = job.source_file.file_name().context("source has no file name")?;
    let dest = free_path(&processed_dir.join(name))?;

    std::fs::rename(&job.source_file, &dest)
        .with_context(|| format!("move source: {} to {}", job.source_file.display(), dest.display()))?;
    remove_emptied_dirs(job);
    Ok(())
}

fn remove_emptied_dirs(job: &Job) {
    let Some(profile) = job.encode_toml.parent() else { return };
    let mut dir = job.source_file.parent();
    while let Some(d) = dir.filter(|d| *d != profile && d.starts_with(profile)) {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// `path` if free, else the same name with a `.2`, `.3`, ... before the extension.
fn free_path(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        return Ok(path.to_path_buf());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();

    for n in 2..1000u32 {
        let candidate = dir.join(format!("{stem}.{n}{ext}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("no free name for {} after 1000 tries", path.display())
}

fn encode_one(w: &WorkerCtx, scene: &SceneEntry) -> Result<()> {
    let chunk_key    = scene.padded_index();
    let scene_frames = scene.frame_count();
    let crf_override = resolve_crf(w, &chunk_key, scene)?;

    let overrides  = encode::EncodeOverrides { crf: crf_override, preset: None };
    let t0         = std::time::Instant::now();
    let size_bytes = encode::encode_chunk(
        w.source, &w.temp.index_path, scene, &w.temp.chunk_path(&chunk_key), w.config, w.opts, overrides,
    )?;

    let enc_fps = scene_frames as f64 / t0.elapsed().as_secs_f64();
    w.done.mark_done(&chunk_key, scene_frames, size_bytes)?;

    let n_chunks = w.completed_chunks.fetch_add(1, Ordering::Relaxed) + 1;
    let n_frames = w.completed_frames.fetch_add(scene_frames, Ordering::Relaxed) + scene_frames;
    let pct      = n_frames * 100 / w.total_frames;
    tracing::info!(
        "[{}] chunk {n_chunks}/{} - {pct}% - {enc_fps:.1} fps - {:.1} MB",
        w.stem, w.total_chunks, size_bytes as f64 / 1_048_576.0
    );
    Ok(())
}

/// Cached CRF, else probe-and-solve; None when target quality is off.
fn resolve_crf(w: &WorkerCtx, chunk_key: &str, scene: &SceneEntry) -> Result<Option<f64>> {
    let (Some(tq), Some(display_model), Some(gpu_id), Some(cache)) =
        (&w.config.target_quality, w.tq_display_model, w.tq_gpu_id, &w.crf_cache) else {
        return Ok(None);
    };
    if let Some(c) = cache.get(chunk_key) {
        tracing::info!("[{}] chunk {chunk_key} using cached target crf {c}", w.stem);
        return Ok(Some(c));
    }

    let ctx = target_quality::ProbeContext {
        source: w.source, index: &w.temp.index_path, temp_dir: &w.temp.path,
        config: w.config, opts: w.opts, tq,
        display_model, gpu_id, gpu_lock: &w.gpu_lock,
        source_width: w.source_width, source_height: w.source_height,
        n_threads: w.threads_per_worker, stem: w.stem, source_byte_index: &w.source_byte_index,
    };
    let res = target_quality::solve_chunk_crf(&ctx, scene)?;

    cache.insert(chunk_key, res.crf)?;
    match res.outcome {
        target_quality::SolveOutcome::Met => tracing::info!(
            "[{}] chunk {chunk_key} target crf {} ({}, {:.0}% size)",
            w.stem, res.crf, res.scores(), res.size_pct
        ),
        target_quality::SolveOutcome::CapBinding => tracing::warn!(
            "[{}] chunk {chunk_key} crf {} capped by max_encoded_percent, floor not held ({}, {:.0}% size)",
            w.stem, res.crf, res.scores(), res.size_pct
        ),
        target_quality::SolveOutcome::FloorUnreachable => tracing::warn!(
            "[{}] chunk {chunk_key} floor unreachable, using crf {} ({})",
            w.stem, res.crf, res.scores()
        ),
    }
    Ok(Some(res.crf))
}

pub fn handle_failure(job: &Job, ctx: &JobContext, stem: &str, err: &anyhow::Error) {
    // Still loud - a typo in encode.toml has to be seen - but not a verdict on the file.
    if is_transient(err) {
        tracing::error!("[{stem}] job failed - retrying on the next scan\n{err:#}");
        return;
    }

    tracing::error!("[{stem}] job failed - source kept, temp dir preserved\n{err:#}");

    let temp = TempDir::for_video(&job.output_dir(&ctx.output_dir), stem);
    if let Err(e) = temp.create_dirs() {
        tracing::warn!("[{stem}] could not create temp dir for failure marker: {e:#}");
    }
    // So the marker locks out this file, not the next one with the same name.
    if let Err(e) = std::fs::write(&temp.source_id_path, job.source_file.display().to_string()) {
        tracing::warn!("[{stem}] could not record source path: {e:#}");
    }
    if let Err(e) = std::fs::write(&temp.failed_path, format!("{err:#}")) {
        tracing::warn!("[{stem}] could not write failure marker: {e:#}");
    }
}

fn run_copy(job: &Job, ctx: &JobContext, config: &Config, stem: &str, temp: &TempDir) -> Result<()> {
    let ignored = ignored_video_opts(&config.avet);
    if !ignored.is_empty() {
        tracing::warn!("[{stem}] video = copy: ignoring {}", ignored.join(", "));
    }

    let audio_plan = audio::plan(&job.source_file, &config.audio)?;
    for line in audio_plan.summary_lines() {
        tracing::info!("[{stem}] audio {line}");
    }

    tracing::info!("[{stem}] copy video, processing audio");
    let video = MuxVideo { path: &job.source_file, timestamps: None, remove: false, expected_frames: None };
    finalize(job, ctx, config, temp, &audio_plan, video)
}

fn ignored_video_opts(a: &crate::config::AvetConfig) -> Vec<&'static str> {
    let mut v = Vec::new();
    if a.hdr { v.push("hdr"); }
    if a.crop { v.push("crop"); }
    if a.keyint { v.push("keyint"); }
    if a.scale.is_some() { v.push("scale"); }
    if a.bit_depth.is_some() { v.push("bit_depth"); }
    v
}

/// `(scale_target, crop, scene_vf)`. Crop is in source space and runs before the scale.
fn compute_output_params(
    src_w: u32,
    src_h: u32,
    crop_str: Option<&str>,
    target_height: Option<u32>,
    stem: &str,
) -> (Option<(u32, u32)>, Option<Crop>, Option<String>) {
    // Once, here: Y4M writer, detection filter and FFVship all read this rectangle.
    let src_crop = crop_str.and_then(Crop::from_str).and_then(|c| {
        let n = c.normalized(src_w, src_h);
        if n.is_none() {
            tracing::warn!(
                "[{stem}] ignoring crop {}:{}:{}:{} - does not fit {src_w}x{src_h}",
                c.w, c.h, c.x, c.y
            );
        }
        n
    });

    let (eff_w, eff_h) = match src_crop {
        Some(c) => (c.w, c.h),
        None    => (src_w, src_h),
    };

    let scale_factor: f64 = match target_height {
        Some(th) if eff_h > th => th as f64 / eff_h as f64,
        _ => 1.0,
    };

    let scale_target: Option<(u32, u32)> = (scale_factor < 1.0).then(|| {
        let tw = round_down_even((eff_w as f64 * scale_factor) as u32);
        let th = round_down_even((eff_h as f64 * scale_factor) as u32);
        tracing::info!("[{stem}] auto-scale: {eff_w}x{eff_h} to {tw}x{th} (factor {scale_factor:.4})");
        (tw, th)
    });

    let scene_vf = build_scene_vf(src_crop, scale_target);

    (scale_target, src_crop, scene_vf)
}

/// ffmpeg -vf filter for scene detection (source-space crop + optional scale).
fn build_scene_vf(crop: Option<Crop>, scale_target: Option<(u32, u32)>) -> Option<String> {
    let crop = crop.map(|c| c.to_filter());
    match (crop, scale_target) {
        (None,    None)         => None,
        (Some(c), None)         => Some(c),
        (None,    Some((w, h))) => Some(format!("scale={w}:{h}")),
        (Some(c), Some((w, h))) => Some(format!("{c},scale={w}:{h}")),
    }
}

fn round_down_even(v: u32) -> u32 {
    v & !1
}

/// Stable hash of everything that affects chunk output and scene boundaries.
fn profile_fingerprint(
    encoder: Option<crate::config::Encoder>,
    merged_args: &[String],
    opts: &EncodeOptions,
    scene_cfg: &crate::config::SceneDetectionConfig,
    tq: Option<&TargetQualityConfig>,
) -> String {
    use std::hash::{Hash, Hasher};
    let parts = [
        format!("{encoder:?}"),
        merged_args.join(" "),
        format!("{:?}", opts.scale),
        format!("{:?}", opts.crop),
        format!("{:?}", opts.target_bit_depth),
        format!("{scene_cfg:?}"),
        format!("{tq:?}"),
    ];
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.join("|").hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Index and crop cache survive a profile change; they depend only on the source.
fn invalidate_stale_cache(temp: &TempDir, fingerprint: &str, stem: &str) -> Result<()> {
    let prev = std::fs::read_to_string(&temp.fingerprint_path).ok();
    if prev.as_deref() == Some(fingerprint) {
        return Ok(());
    }
    if prev.is_some() {
        tracing::warn!("[{stem}] encode profile changed, discarding cached scenes and chunks");
        let _ = std::fs::remove_file(&temp.scenes_path);
        let _ = std::fs::remove_file(&temp.done_path);
        let _ = std::fs::remove_file(&temp.tq_path);
        let _ = std::fs::remove_dir_all(&temp.chunks_dir);
        temp.create_dirs()?;
    }
    std::fs::write(&temp.fingerprint_path, fingerprint)
        .with_context(|| format!("write {}", temp.fingerprint_path.display()))
}

/// Size and mtime have to hold still for 3 s: NFS caches attributes for `acregmin`,
/// so a shorter look at the size alone reads the same value twice.
fn wait_for_stable(path: &Path, stem: &str) -> Result<()> {
    const TIMEOUT_SECS: u64 = 300;
    const INTERVAL_SECS: u64 = 3;

    let missing = || {
        anyhow::Error::new(Transient)
            .context(format!("file is empty or missing: {}", path.display()))
    };

    let mut state = file_state(path);
    if state.0 == 0 {
        return Err(missing());
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(TIMEOUT_SECS);
    let mut announced = false;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(INTERVAL_SECS));
        let next = file_state(path);
        // Moved away mid-wait it reads as (0, None) twice, which compares as stable.
        if next.0 == 0 {
            return Err(missing());
        }
        if next == state {
            return Ok(());
        }
        if !announced {
            tracing::info!("[{stem}] file is still being written - waiting...");
            announced = true;
        }
        state = next;

        if std::time::Instant::now() >= deadline {
            return Err(anyhow::Error::new(Transient).context(format!(
                "still growing after {TIMEOUT_SECS}s: {}",
                path.display()
            )));
        }
    }
}

/// The pair that has to hold still for a copy to be done.
fn file_state(path: &Path) -> (u64, Option<std::time::SystemTime>) {
    match std::fs::metadata(path) {
        Ok(m) => (m.len(), m.modified().ok()),
        Err(_) => (0, None),
    }
}

/// Cumulative source bytes by frame; empty on failure, which disables the size cap.
fn probe_source_byte_index(source: &Path, stem: &str) -> Vec<u64> {
    #[derive(serde::Deserialize)]
    struct Packets { #[serde(default)] packets: Vec<Pkt> }
    #[derive(serde::Deserialize)]
    struct Pkt { #[serde(default)] size: Option<String> }

    // Demuxes the whole file, and a killed probe disables the cap silently.
    const TIMEOUT_SECS: u64 = 3600;

    let parsed: Packets = match crate::ext::ffprobe_json_with_timeout(
        &["-v", "error", "-select_streams", "v:0",
          "-show_entries", "packet=size", "-of", "json"],
        source,
        TIMEOUT_SECS,
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("[{stem}] source packet-size probe failed: {e:#} - size cap disabled");
            return Vec::new();
        }
    };

    let mut cum = Vec::with_capacity(parsed.packets.len() + 1);
    let mut acc = 0u64;
    cum.push(0);
    for pk in &parsed.packets {
        acc += pk.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);
        cum.push(acc);
    }
    cum
}

/// Rebased to 0, or None while every frame is within half a frame of the encoder's rate.
fn vfr_timestamps(ts: &[f64], fps_num: u32, fps_den: u32, stem: &str) -> Option<Vec<f64>> {
    let first = *ts.first()?;
    let frame_ms = 1000.0 * fps_den as f64 / fps_num as f64;
    let rebased: Vec<f64> = ts.iter().map(|t| t - first).collect();

    let constant = rebased
        .iter()
        .enumerate()
        .all(|(i, t)| (t - i as f64 * frame_ms).abs() <= frame_ms / 2.0);
    if constant {
        return None;
    }
    if !rebased.windows(2).all(|w| w[1] > w[0]) {
        tracing::warn!("[{stem}] source timestamps are not increasing - using a constant frame rate");
        return None;
    }
    Some(rebased)
}

fn write_timestamps(path: &Path, ts: &[f64]) -> Result<()> {
    use std::fmt::Write;
    let mut text = String::from("# timestamp format v2\n");
    for t in ts {
        let _ = writeln!(text, "{t:.3}");
    }
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

fn probe_fps(source: &Path) -> Result<(u32, u32)> {
    #[derive(serde::Deserialize)]
    struct Probe { streams: Vec<Stream> }
    #[derive(serde::Deserialize)]
    struct Stream { avg_frame_rate: String }

    let p: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "v:0",
          "-show_entries", "stream=avg_frame_rate", "-of", "json"],
        source,
    )?;
    let rate = p.streams.into_iter().next()
        .map(|s| s.avg_frame_rate)
        .context("ffprobe found no video stream")?;

    if let Some((n, d)) = rate.split_once('/') {
        let n: u32 = n.trim().parse().context("parse fps numerator")?;
        let d: u32 = d.trim().parse().context("parse fps denominator")?;
        if d > 0 && n > 0 { Ok((n, d)) } else { bail!("invalid fps: {n}/{d}") }
    } else {
        let n: u32 = rate.trim().parse().context("parse fps")?;
        if n > 0 { Ok((n, 1)) } else { bail!("invalid fps: {n}") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SceneDetectionConfig;

    fn opts() -> EncodeOptions {
        EncodeOptions { fps_num: 24, fps_den: 1, ..Default::default() }
    }

    #[test]
    fn fingerprint_changes_with_profile() {
        use crate::config::Encoder;
        let args = vec!["--crf".to_string(), "28".to_string()];
        let sc = SceneDetectionConfig::default();
        let enc = Some(Encoder::SvtAv1);
        let base = profile_fingerprint(enc, &args, &opts(), &sc, None);

        assert_eq!(base, profile_fingerprint(enc, &args, &opts(), &sc, None));

        // Resuming across a switch would merge one binary's chunks into the other's.
        assert_ne!(base, profile_fingerprint(Some(Encoder::SvtAv1Hdr), &args, &opts(), &sc, None));

        let args2 = vec!["--crf".to_string(), "30".to_string()];
        assert_ne!(base, profile_fingerprint(enc, &args2, &opts(), &sc, None));

        let mut o = opts();
        o.scale = Some((1920, 1080));
        assert_ne!(base, profile_fingerprint(enc, &args, &o, &sc, None));

        let tq = crate::config::TargetQualityConfig { jod: 9.6, ..Default::default() };
        assert_ne!(base, profile_fingerprint(enc, &args, &opts(), &sc, Some(&tq)));
    }
}

#[cfg(test)]
mod failure_class_tests {
    use super::*;

    #[test]
    fn transient_survives_both_construction_orders() {
        // As a context on a real error, which is how a bad profile is reported.
        let as_context = Err::<(), _>(anyhow::anyhow!("parse encode.toml"))
            .context(Transient)
            .unwrap_err();
        assert!(is_transient(&as_context));

        // As the base error, which is how wait_for_stable reports an unfinished copy.
        let as_base = anyhow::Error::new(Transient).context("file is empty or missing");
        assert!(is_transient(&as_base));

        let wrapped = Err::<(), _>(as_base).context("run job").unwrap_err();
        assert!(is_transient(&wrapped));
    }

    #[test]
    fn a_rejected_profile_is_transient_through_the_real_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml = dir.path().join("encode.toml");
        std::fs::write(&toml, "encoder = \"svt-av1\"\n[avet]\nscale = 0\n").unwrap();

        let err = Config::from_file(&toml).context(Transient).unwrap_err();
        assert!(err.to_string().contains("retrying") || format!("{err:#}").contains("scale"));
        assert!(is_transient(&err), "got: {err:#}");
    }

    #[test]
    fn an_ordinary_failure_is_not_transient() {
        let err = Err::<(), _>(anyhow::anyhow!("encoder failed"))
            .context("chunk 00007")
            .unwrap_err();
        assert!(!is_transient(&err));
    }
}

#[cfg(test)]
mod output_param_tests {
    use super::*;

    #[test]
    fn crop_is_normalized_before_anything_downstream_sees_it() {
        // Unrounded, the metric tool and the encoder compared frames a line apart.
        let (scale, crop, vf) = compute_output_params(1920, 1080, Some("crop=1920:801:0:141"), None, "t");
        let crop = crop.expect("crop should survive normalization");
        assert_eq!((crop.w, crop.h, crop.x, crop.y), (1920, 800, 0, 140));
        assert_eq!(vf.as_deref(), Some("crop=1920:800:0:140"));
        assert_eq!(scale, None);
    }

    #[test]
    fn crop_larger_than_the_source_is_dropped() {
        let (_, crop, vf) = compute_output_params(1280, 720, Some("crop=1920:800:0:140"), None, "t");
        assert_eq!(crop, None);
        assert_eq!(vf, None);
    }

    #[test]
    fn scale_applies_to_the_cropped_size_and_keeps_even_edges() {
        let (scale, crop, vf) = compute_output_params(1920, 1080, Some("crop=1920:800:0:140"), Some(400), "t");
        assert_eq!(crop.map(|c| (c.w, c.h)), Some((1920, 800)));
        assert_eq!(scale, Some((960, 400)));
        assert_eq!(vf.as_deref(), Some("crop=1920:800:0:140,scale=960:400"));
    }

    #[test]
    fn scale_above_the_source_height_is_not_an_upscale() {
        let (scale, _, vf) = compute_output_params(1280, 720, None, Some(1080), "t");
        assert_eq!(scale, None);
        assert_eq!(vf, None);
    }

    #[test]
    fn archiving_never_overwrites_an_earlier_source() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = dir.path().join("Episode 01.mkv");

        assert_eq!(free_path(&first).unwrap(), first);

        std::fs::write(&first, b"season 1").unwrap();
        let second = free_path(&first).unwrap();
        assert_eq!(second, dir.path().join("Episode 01.2.mkv"));

        std::fs::write(&second, b"season 2").unwrap();
        assert_eq!(free_path(&first).unwrap(), dir.path().join("Episode 01.3.mkv"));

        assert_eq!(std::fs::read(&first).unwrap(), b"season 1");
    }

    #[test]
    fn archiving_the_last_episode_removes_its_empty_folders_but_not_the_profile() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("input").join("p");
        let season = profile.join("Show").join("Season 1");
        std::fs::create_dir_all(&season).unwrap();
        std::fs::write(profile.join("encode.toml"), b"").unwrap();
        std::fs::write(season.join("Episode 01.mkv"), b"e1").unwrap();
        std::fs::write(season.join("Episode 02.mkv"), b"e2").unwrap();

        let ctx = JobContext { input_dir: dir.path().join("input"), output_dir: dir.path().join("output") };
        let job = |name: &str| Job {
            encode_toml: profile.join("encode.toml"),
            source_file: season.join(name),
            rel_dir: PathBuf::from("Show/Season 1"),
        };

        archive_source(&job("Episode 01.mkv"), &ctx).unwrap();
        assert!(season.is_dir(), "a folder still holding a video was removed");

        archive_source(&job("Episode 02.mkv"), &ctx).unwrap();
        assert!(!profile.join("Show").exists());
        assert!(profile.is_dir());
        let archived = ctx.input_dir.join("processed/Show/Season 1");
        assert_eq!(std::fs::read(archived.join("Episode 02.mkv")).unwrap(), b"e2");
    }

    #[test]
    fn only_timestamps_that_drift_from_the_frame_rate_are_kept() {
        // 23.976 fps in a millisecond timebase: rounding, not a variable rate.
        let cfr: Vec<f64> = (0..10_000).map(|i| (i as f64 * 1001.0 / 24.0).round() + 80.0).collect();
        assert_eq!(vfr_timestamps(&cfr, 24000, 1001, "t"), None);

        let vfr: Vec<f64> = (0..30).map(|i| i as f64 * 1000.0 / 30.0)
            .chain((0..60).map(|i| 1000.0 + i as f64 * 1000.0 / 60.0))
            .map(|t| t + 500.0)
            .collect();
        let kept = vfr_timestamps(&vfr, 45, 1, "t").expect("variable rate not detected");
        assert_eq!(kept[0], 0.0);
        assert!((kept[30] - 1000.0).abs() < 1e-9);

        let mut backwards = vfr.clone();
        backwards.swap(40, 41);
        assert_eq!(vfr_timestamps(&backwards, 45, 1, "t"), None);
        assert_eq!(vfr_timestamps(&[], 24, 1, "t"), None);
    }

    #[test]
    fn a_source_that_disappears_mid_wait_is_transient() {
        // The file is fine; a marker would lock it out for good once it came back.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("film.mkv");
        std::fs::write(&path, b"data").unwrap();

        let gone = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            std::fs::remove_file(&gone).unwrap();
        });

        let err = wait_for_stable(&path, "film").unwrap_err();
        assert!(is_transient(&err), "got: {err:#}");
    }

    #[test]
    fn a_timeout_is_transient_but_a_bad_profile_stays_permanent() {
        let timeout = anyhow::Error::new(Transient).context("ffprobe did not finish within 120s");
        assert!(is_transient(&timeout.context("HDR detection")));

        let disk_full = anyhow::Error::new(std::io::Error::from(
            std::io::ErrorKind::StorageFull,
        ))
        .context("write chunk");
        assert!(is_transient(&disk_full));

        let real = anyhow::anyhow!("encoder exited with status 1");
        assert!(!is_transient(&real.context("encode chunk 3")));
    }
}
