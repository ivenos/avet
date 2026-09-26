use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::audio;
use crate::config::{Config, Encoder, TargetQualityConfig, VideoMode};
use crate::encode::{self, EncodeOptions};
use crate::ffms2::{self, Crop};
use crate::hdr::DynamicHdr;
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
pub fn is_transient(err: &anyhow::Error) -> bool {
    if err.downcast_ref::<Transient>().is_some() {
        return true;
    }
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| matches!(io.kind(), std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded))
    })
}

struct WorkerCtx<'a> {
    source: &'a Path,
    temp: &'a TempDir,
    config: &'a Config,
    opts: &'a EncodeOptions,
    tq_display_model: Option<target_quality::DisplayModel>,
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
    // Before indexing and scene detection, which a setup without a GPU would pay every scan.
    let gpu = match &config.target_quality {
        Some(_) => Some(target_quality::ensure_available().context(Transient)?),
        None => None,
    };

    let stem = job.stem();

    wait_for_stable(&job.source_file, stem)?;

    let temp = TempDir::for_video(&job.output_dir(&ctx.output_dir), stem);
    temp.claim_source(&job.source_file, stem)?;

    if config.avet.video == VideoMode::Copy {
        return run_copy(job, ctx, &config, stem, &temp);
    }

    let audio_plan = audio::plan(&job.source_file, &config.audio)?;
    for line in audio_plan.summary_lines() {
        tracing::info!("[{stem}] audio {line}");
    }
    audio::check_encoders(&job.source_file, &audio_plan)?;

    let video_file = frame_accurate_source(&job.source_file, &temp, stem)?;
    let video_source = open_indexed(&video_file, &temp.index_path, stem)?;
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

    let source_video = probe_source_video(&job.source_file)?;
    let (fps_num, fps_den) = (source_video.fps_num, source_video.fps_den);
    let fps = fps_num as f64 / fps_den as f64;

    // SvtAv1EncApp refuses more than 240 fps, so it gets a fraction and the timestamps the rest.
    let fps_divisor = if config.encoder == Some(Encoder::SvtAv1) && fps > 240.0 { (fps / 240.0).ceil() as u32 } else { 1 };
    let timestamps = match vfr_timestamps(&source_timestamps, fps_num, fps_den, stem) {
        Some(ts) => {
            tracing::info!("[{stem}] variable frame rate: keeping the source timestamps");
            write_timestamps(&temp.timestamps_path, &ts, source_video.offset_ms)?;
            Some(temp.timestamps_path.as_path())
        }
        None if fps_divisor > 1 => {
            tracing::info!("[{stem}] {fps:.3} fps: encoding at a 1/{fps_divisor} rate, the source timestamps keep the real one");
            let first = source_timestamps.first().copied().unwrap_or(0.0);
            let ts: Vec<f64> = source_timestamps.iter().map(|t| t - first).collect();
            write_timestamps(&temp.timestamps_path, &ts, source_video.offset_ms)?;
            Some(temp.timestamps_path.as_path())
        }
        None => None,
    };

    let hdr = crate::hdr::detect(&job.source_file)?;
    let chroma_center = hdr.chroma_center;
    let hevc_source = hdr.codec_name == "hevc";
    // FFMS2 hands out the RPU of HEVC only, and AV1 has a form for profiles 7 and 8 only.
    let dv = config.avet.dv && hevc_source && hdr.dv_profile.is_none_or(|p| matches!(p, 7 | 8));
    if config.avet.dv && !hevc_source && hdr.hdr_type == "Dolby Vision" {
        tracing::warn!("[{stem}] HDR: Dolby Vision is only carried from HEVC, not from {}", hdr.codec_name);
    }
    // Profile 5's base layer is IPT-PQ-C2, an image only once the RPU is applied.
    if hdr.ipt_base_layer() {
        bail!(
            "this Dolby Vision stream (profile {}) has an IPT base layer: its picture exists \
             only with the RPU applied, which avet does not do. Convert the source to HDR10 \
             first, for example with ffmpeg's libplacebo filter.",
            hdr.dv_profile.map_or("unknown".into(), |p| p.to_string())
        );
    }
    match (hdr.dv_profile, hdr.hdr_type.as_str()) {
        (Some(p), _) if dv => tracing::info!(
            "[{stem}] HDR: Dolby Vision profile {p} (carried as AV1 profile 10)"
        ),
        (Some(p), _) => tracing::info!(
            "[{stem}] HDR: Dolby Vision profile {p} (RPU dropped, base layer kept)"
        ),
        (None, "Dolby Vision") if dv => tracing::warn!(
            "[{stem}] HDR: Dolby Vision of unknown profile (carried as AV1 profile 10)"
        ),
        (None, "Dolby Vision") => tracing::warn!(
            "[{stem}] HDR: Dolby Vision of unknown profile (RPU dropped; the base layer \
             is only a valid picture on profiles 7 and 8)"
        ),
        (None, "HDR10+") => tracing::info!(
            "[{stem}] HDR: HDR10+ (dynamic metadata carried)"
        ),
        (None, "SDR") => {}
        (None, t) => tracing::info!("[{stem}] HDR: {t}"),
    }
    if hdr.hdr10plus && hdr.hdr_type != "HDR10+" {
        tracing::info!("[{stem}] HDR: also HDR10+ (dynamic metadata carried)");
    }
    let missing = hdr.missing_static_metadata();
    if !missing.is_empty() {
        tracing::warn!("[{stem}] HDR metadata incomplete - missing: {}", missing.join(", "));
    }
    // Only what the source has: a profile's HDR10 encodes keep their fingerprint.
    let dynamic_hdr = DynamicHdr {
        hdr10plus: hdr.hdr10plus,
        dolby_vision: dv && hdr.hdr_type == "Dolby Vision",
    };
    let hdr_args = hdr.encoder_args();

    let hdr10plus_frames = if dynamic_hdr.hdr10plus && hevc_source {
        tracing::info!("[{stem}] HDR10+: reading it from the bitstream");
        match crate::hevc::hdr10plus_frames(&job.source_file, &temp.hdr10plus_path) {
            Ok(frames) if frames.len() == video_info.num_frames as usize => Some(frames),
            Ok(frames) => {
                tracing::warn!(
                    "[{stem}] HDR10+: the bitstream holds {} pictures, the index {} - using the decoder's values",
                    frames.len(), video_info.num_frames
                );
                None
            }
            Err(e) if is_transient(&e) => return Err(e),
            Err(e) => {
                tracing::warn!("[{stem}] HDR10+: could not read it from the bitstream - using the decoder's values: {e:#}");
                None
            }
        }
    } else {
        None
    };

    let duration_secs = video_info.num_frames as f64 / fps;
    let crop_str: Option<String> = if config.avet.crop {
        crate::crop::detect(&video_file, duration_secs, &temp.crop_cache, stem)?
    } else {
        None
    };
    let deinterlace = crate::interlace::detect(&video_file, duration_secs, &temp.interlace_cache, stem)?;

    let (scale_target, crop, scene_vf) = compute_output_params(
        video_info.width,
        video_info.height,
        crop_str.as_deref(),
        config.avet.scale,
        deinterlace,
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
        deinterlace,
        fps_num,
        fps_den: fps_den * fps_divisor,
        target_bit_depth: config.avet.bit_depth,
        dynamic_hdr,
        hdr10plus_frames,
    };

    let merged_args = encode::merged_encoder_args(&config, &encode_opts);

    // So a resumed encode never mixes chunks from different settings. The crf is left out
    // under target_quality: there it only seeds the probe search and never reaches a chunk.
    let fingerprint_args: Vec<String> = if config.target_quality.is_some() {
        without_arg(&merged_args, "--crf")
    } else {
        merged_args.clone()
    };
    let fingerprint = profile_fingerprint(
        config.encoder, &fingerprint_args, &encode_opts, &config.scene_detection,
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
        let mut scenes = scene::detect(
            &video_file,
            &config.scene_detection,
            scene_vf.as_deref(),
            fps,
        )?;
        let detected = scenes.last().map_or(0, |s| s.end_frame + 1);
        let gap = (video_info.num_frames as u64).saturating_sub(detected);
        if gap > 0 && leading_frames(&video_file, &source_timestamps, gap)? == gap {
            tracing::info!("[{stem}] the first {gap} frames do not decode on their own: scene cuts move by as many");
            shift_scenes(&mut scenes, gap);
        }
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
    validate_scene_list(&scenes, ffms2_frames)?;

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

    // FFVship compares at source resolution, so the model follows the crop, not the scale.
    let (reference_width, reference_height) = encode_opts
        .crop
        .map(|c| (c.w, c.h))
        .unwrap_or((video_info.width, video_info.height));
    let (tq_display_model, tq_gpu_id, crf_cache): (Option<target_quality::DisplayModel>, Option<u32>, Option<CrfCache>) =
        if let (Some(tq), Some(gpu)) = (&config.target_quality, &gpu) {
            let display_model = target_quality::display_model_for(
                reference_width, reference_height, (video_info.width, video_info.height), &encode_opts.hdr_args,
            );
            tracing::info!(
                "[{stem}] target quality: JOD {} floor (display {}, {}, crf {}-{}, {}-{} probes, probe preset {}, max {}% size)",
                tq.jod, display_model.describe(), gpu.describe(), tq.min_crf, tq.max_crf,
                tq.min_probes, tq.max_probes, tq.probe_preset, tq.max_encoded_percent
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
            sweep_probe_leftovers(&temp.path, stem);
            (Some(display_model), Some(gpu.id), Some(CrfCache::load_or_create(&temp.tq_path)?))
        } else {
            (None, None, None)
        };

    tracing::info!("[{stem}] encoding: {total_chunks} chunks, {num_workers} worker(s)");

    let source_byte_index = if config.target_quality.is_some() {
        probe_source_byte_index(&job.source_file, ffms2_frames, stem)?
    } else {
        Vec::new()
    };

    let wctx = WorkerCtx {
        source: &video_file,
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
        if wctx.done.is_done(&chunk_key, &temp.chunk_path(&chunk_key), scene.frame_count()) {
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
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| encode_one(&wctx, scene)))
                    .unwrap_or_else(|panic| Err(anyhow::anyhow!(
                        "chunk {:05} panicked: {}", scene.index + 1, crate::panic_message(&*panic)
                    )));
                if let Err(e) = result {
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
    let joined = crate::av1::concat_ivf(&chunk_paths, &video_only)?;
    if joined != total_frames {
        bail!(
            "the joined chunks hold {joined} frames, the chunk list accounts for {total_frames}. \
             Delete the job's temp dir to encode it again from scratch."
        );
    }

    tracing::info!("[{stem}] processing audio");
    let mut video_args = crate::hdr::mkvmerge_color_args(&merged_args, 0);
    if chroma_center {
        video_args.extend(["--chroma-siting".into(), "0:2,2".into()]);
    }
    // mkvmerge ignores --sync on a track that gets a timestamps file, which carries it instead.
    if timestamps.is_none() && source_video.offset_ms != 0 {
        video_args.extend(["--sync".into(), format!("0:{}", source_video.offset_ms)]);
    }
    let (out_w, out_h) = scale_target
        .or(encode_opts.crop.map(|c| (c.w, c.h)))
        .unwrap_or((video_info.width, video_info.height));
    video_args.extend(source_video.display_args(out_w, out_h));
    let video = MuxVideo {
        path: &video_only,
        args: video_args,
        timestamps,
        source_shift_ms: -source_video.matroska_start_ms,
        remove: true,
        expected_frames: Some(total_frames),
    };
    finalize(job, ctx, &config, &temp, &audio_plan, video)
}

/// Frames FFMS2 counts before the first one ffmpeg decodes, such as the open-GOP leftovers at
/// the start of a recording. The scene detector numbers its frames from after them.
fn leading_frames(source: &Path, timestamps_ms: &[f64], gap: u64) -> Result<u64> {
    #[derive(serde::Deserialize)]
    struct Probe { #[serde(default)] frames: Vec<Frame> }
    #[derive(serde::Deserialize)]
    struct Frame { best_effort_timestamp_time: Option<String> }

    let packets = format!("%+#{}", gap + 100);
    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "quiet", "-select_streams", "v:0", "-read_intervals", &packets,
          "-show_entries", "frame=best_effort_timestamp_time", "-of", "json"],
        source,
    )
    .context("probe the first decoded frame")?;
    let Some(first_ms) = probe.frames.first()
        .and_then(|f| f.best_effort_timestamp_time.as_deref()?.parse::<f64>().ok())
        .map(|s| s * 1000.0)
    else {
        return Ok(0);
    };
    Ok(timestamps_ms.iter().take_while(|&&t| t < first_ms - 0.5).count() as u64)
}

/// The first chunk takes the leading frames, every later cut moves by as many.
fn shift_scenes(scenes: &mut [SceneEntry], by: u64) {
    for (i, s) in scenes.iter_mut().enumerate() {
        if i > 0 {
            s.start_frame += by;
        }
        s.end_frame += by;
    }
}

/// `total_frames` is summed from this list and `validate_output` checks the finished file
/// against that sum, so a gap between two chunks would pass as a complete encode.
fn validate_scene_list(scenes: &[SceneEntry], frames: u64) -> Result<()> {
    let mut next = 0;
    for s in scenes {
        if s.end_frame < s.start_frame {
            bail!("chunk {} ends at frame {} before its start {}", s.index, s.end_frame, s.start_frame);
        }
        if s.start_frame != next {
            bail!(
                "chunk {} starts at frame {}, but the one before it ended at {next}. The \
                 chunk list has a gap or an overlap - delete the job's temp dir to detect \
                 the scenes again.",
                s.index, s.start_frame
            );
        }
        next = s.end_frame + 1;
    }
    if next != frames {
        bail!(
            "the chunk list covers {next} frames, the source has {frames}. Delete the job's \
             temp dir to detect the scenes again."
        );
    }
    Ok(())
}

struct MuxVideo<'a> {
    /// The merged encode (run), or the source or its Matroska copy (run_copy).
    path: &'a Path,
    args: Vec<String>,
    source_shift_ms: i64,
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
    let subtitles = crate::subtitle::plan(&job.source_file, &config.subtitles)?;
    audio::extract(&job.source_file, &temp.tracks_path, audio_plan, &subtitles.extract)?;
    let languages = audio::Languages::probe(&job.source_file)?;

    let final_output = job.output_dir(&ctx.output_dir).join(format!("{stem}.mkv"));
    // An empty file is a leftover, not a result; the scanner ignores it for the same reason.
    match std::fs::metadata(&final_output) {
        Ok(m) if m.len() > 0 => bail!("output already exists: {}", final_output.display()),
        Ok(_) => tracing::warn!("[{stem}] replacing empty {}", final_output.display()),
        Err(_) => {}
    }

    // Into the temp dir first: the next scan reads a half-written output as "already done".
    tracing::info!("[{stem}] muxing to {}", final_output.display());
    audio::mux_final(
        video.path, &video.args, video.timestamps,
        &temp.tracks_path, &languages.of_tracks(audio_plan, &subtitles.extract),
        &job.source_file, languages.video(),
        video.source_shift_ms, &subtitles.from_source, &temp.mux_path,
    )?;
    if video.expected_frames.is_none() {
        realign_copied_video(&job.source_file, temp, stem)?;
    }
    if let Err(e) = crate::mkv::trim_av1_codec_private(&temp.mux_path) {
        tracing::warn!("[{stem}] could not remove frame metadata from the AV1 codec private data: {e:#}");
    }

    if video.remove {
        let _ = std::fs::remove_file(video.path);
    }

    tracing::info!("[{stem}] validating output");
    encode::validate_output(&temp.mux_path, video.expected_frames)?;

    if !temp.source_unchanged(&job.source_file) {
        return Err(anyhow::Error::new(Transient)
            .context(format!("{} changed during the encode", job.source_file.display())));
    }
    // mkvmerge does not fsync, and the source is archived right after the rename.
    std::fs::File::open(&temp.mux_path)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("flush {}", temp.mux_path.display()))?;
    std::fs::rename(&temp.mux_path, &final_output).with_context(|| {
        format!("move {} to {}", temp.mux_path.display(), final_output.display())
    })?;

    // Delivered: the scanner short-circuits on "output exists", so nothing below retries. The
    // temp dir goes first, before a copy to another disk that a stop could cut short.
    if !config.avet.keep_temp
        && let Err(e) = std::fs::remove_dir_all(&temp.path)
    {
        tracing::error!("[{stem}] could not remove temp dir {}: {e:#}", temp.path.display());
    }

    if let Err(e) = archive_source(job, ctx) {
        tracing::error!("[{stem}] output is in place, but archiving the source failed: {e:#}");
    }

    tracing::info!("[{stem}] done");
    Ok(())
}

/// An index from another FFMS2 or FFmpeg build no longer opens; indexing again keeps the chunks.
fn open_indexed(video_file: &Path, index: &Path, stem: &str) -> Result<ffms2::VideoSource> {
    if index.exists() {
        tracing::info!("[{stem}] reusing existing index");
        match ffms2::VideoSource::open(video_file, index, ffms2::OpenOpts::default()) {
            Ok(vs) => return Ok(vs),
            Err(e) => tracing::warn!("[{stem}] the existing index does not open - indexing again: {e:#}"),
        }
    }
    tracing::info!("[{stem}] indexing");
    ffms2::run_ffmsindex(video_file, index)?;
    tracing::info!("[{stem}] indexing done");
    ffms2::VideoSource::open(video_file, index, ffms2::OpenOpts::default())
}

/// FFMS2 puts the frames of AVI video with runs of B-frames out of order, having only decode
/// times to go by; with timestamps generated into a Matroska copy it does not.
fn frame_accurate_source(source: &Path, temp: &TempDir, stem: &str) -> Result<PathBuf> {
    #[derive(serde::Deserialize)]
    struct Probe { #[serde(default)] streams: Vec<Stream>, #[serde(default)] format: Format }
    #[derive(serde::Deserialize)]
    struct Stream { #[serde(default)] has_b_frames: u32 }
    #[derive(serde::Deserialize, Default)]
    struct Format { #[serde(default)] format_name: String }

    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "v:0",
          "-show_entries", "stream=has_b_frames:format=format_name", "-of", "json"],
        source,
    )
    .context("probe the container for B-frames")?;
    let Some(video) = probe.streams.first() else { return Ok(source.to_path_buf()) };
    if probe.format.format_name != "avi" || video.has_b_frames == 0 {
        return Ok(source.to_path_buf());
    }
    if temp.remux_path.exists() {
        return Ok(temp.remux_path.clone());
    }

    tracing::info!("[{stem}] AVI with B-frames: working from a Matroska copy of the video");
    let _ = std::fs::remove_file(&temp.index_path);
    let part = temp.remux_path.with_extension("mkv.part");
    let mut cmd = std::process::Command::new(crate::ext::external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-fflags", "+genpts", "-i"])
        .arg(source)
        .args(["-map", "0:v:0", "-c", "copy", "-f", "matroska"])
        .arg(&part);
    let out = crate::ext::output_with_timeout(&mut cmd, crate::ext::whole_file_timeout(source, 3600), "ffmpeg remux")?;
    if !out.status.success() {
        return Err(crate::ext::tool_error("ffmpeg remux", out.status, &String::from_utf8_lossy(&out.stderr)));
    }
    std::fs::File::open(&part)
        .and_then(|f| f.sync_all())
        .and_then(|()| std::fs::rename(&part, &temp.remux_path))
        .with_context(|| format!("move {} to {}", part.display(), temp.remux_path.display()))?;
    Ok(temp.remux_path.clone())
}

/// mkvmerge's time zero for a copied FLV, MP4 edit list or wrapping MPEG-TS is not ffmpeg's,
/// which the audio and extracted subtitles are on.
fn realign_copied_video(source: &Path, temp: &TempDir, stem: &str) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Format { #[serde(default)] format: FormatStart }
    #[derive(serde::Deserialize, Default)]
    struct FormatStart { start_time: Option<String> }

    let format: Format = crate::ext::ffprobe_json(
        &["-v", "error", "-show_entries", "format=start_time", "-of", "json"],
        source,
    )
    .context("probe the source start time")?;
    let source_start = format.format.start_time.and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    let (Some(first), Some(actual)) = (first_video_pts(source)?, first_video_pts(&temp.mux_path)?) else {
        return Ok(());
    };
    // Can be negative: frames an MP4 edit list skips still play from the copied stream.
    let expected = first - source_start;
    let lead = (-expected).max(0.0);
    let video_ms = ((expected + lead - actual) * 1000.0).round() as i64;
    let others_ms = (lead * 1000.0).round() as i64;
    if video_ms.abs() <= 1 && others_ms == 0 {
        return Ok(());
    }
    tracing::info!("[{stem}] moving the copied video by {video_ms} ms and the rest by {others_ms} ms");

    let muxed = track_types(&temp.mux_path)?;
    let extracted = if temp.tracks_path.exists() { track_types(&temp.tracks_path)?.len() } else { 0 };
    let videos = muxed.iter().take_while(|t| *t == "video").count();

    let realigned = temp.path.join("realigned.mkv");
    let mut cmd = std::process::Command::new(crate::ext::external_bin("mkvmerge"));
    cmd.arg("-o").arg(&realigned);
    for id in 0..muxed.len() {
        let ms = if id >= videos && id < videos + extracted { others_ms } else { video_ms };
        cmd.arg("--sync").arg(format!("{id}:{ms}"));
    }
    if others_ms != 0 && crate::audio::has_chapters(&temp.mux_path)? {
        cmd.arg("--chapter-sync").arg(others_ms.to_string());
    }
    cmd.arg(&temp.mux_path);
    let out = crate::ext::output_with_timeout(&mut cmd, crate::ext::whole_file_timeout(&temp.mux_path, 3600), "mkvmerge")?;
    if out.status.code().unwrap_or(2) >= 2 {
        return Err(crate::ext::tool_error("mkvmerge", out.status, &String::from_utf8_lossy(&out.stdout)));
    }
    std::fs::rename(&realigned, &temp.mux_path)
        .with_context(|| format!("move {} to {}", realigned.display(), temp.mux_path.display()))
}

/// The earliest time among the first packets; AVI gives presentation times to B-frames alone.
fn first_video_pts(path: &Path) -> Result<Option<f64>> {
    #[derive(serde::Deserialize)]
    struct Packets { #[serde(default)] packets: Vec<Packet> }
    #[derive(serde::Deserialize)]
    struct Packet { pts_time: Option<String>, dts_time: Option<String> }

    let probe: Packets = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "v:0", "-read_intervals", "%+#32",
          "-show_entries", "packet=pts_time,dts_time", "-of", "json"],
        path,
    )
    .with_context(|| format!("probe the first video timestamps of {}", path.display()))?;
    let earliest = |time: fn(&Packet) -> Option<&String>| {
        probe.packets.iter().filter_map(|p| time(p)?.parse::<f64>().ok()).reduce(f64::min)
    };
    if probe.packets.iter().all(|p| p.pts_time.is_some()) {
        Ok(earliest(|p| p.pts_time.as_ref()))
    } else {
        Ok(earliest(|p| p.dts_time.as_ref()))
    }
}

fn track_types(path: &Path) -> Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Identify { tracks: Vec<Track> }
    #[derive(serde::Deserialize)]
    struct Track { #[serde(rename = "type")] track_type: String }

    let mut cmd = std::process::Command::new(crate::ext::external_bin("mkvmerge"));
    cmd.args(["--identify", "--identification-format", "json"]).arg(path);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "mkvmerge --identify")?;
    if out.status.code().unwrap_or(2) >= 2 {
        return Err(crate::ext::tool_error("mkvmerge identify", out.status, &String::from_utf8_lossy(&out.stdout)));
    }
    let identify: Identify = serde_json::from_slice(&out.stdout).context("parse mkvmerge identify output")?;
    Ok(identify.tracks.into_iter().map(|t| t.track_type).collect())
}

/// Never over an existing file: two seasons can each have an `Episode 01.mkv`.
fn archive_source(job: &Job, ctx: &JobContext) -> Result<()> {
    let processed_dir = crate::scanner::ensure_processed_dir(&ctx.input_dir, &job.rel_dir)?;
    let name = job.source_file.file_name().context("source has no file name")?;
    let dest = free_path(&processed_dir.join(name))?;

    move_file(&job.source_file, &dest)
        .with_context(|| format!("move source: {} to {}", job.source_file.display(), dest.display()))?;
    remove_emptied_dirs(job);
    Ok(())
}

/// A profile or `processed/` can be a mount of its own, and rename(2) stops at one.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
        other => return Ok(other?),
    }
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let copied = std::fs::copy(from, &part)
        .and_then(|_| std::fs::File::open(&part)?.sync_all())
        .and_then(|()| std::fs::rename(&part, to));
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&part);
        return Err(e).context("copy across file systems");
    }
    std::fs::remove_file(from).context("remove the original after copying it")
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

    for n in 2..=1000u32 {
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

    if let Some(tq) = &w.config.target_quality {
        let pct = target_quality::chunk_size_pct(size_bytes, &w.source_byte_index, scene.start_frame, scene.end_frame);
        if pct > tq.max_encoded_percent {
            tracing::warn!("[{}] chunk {chunk_key} came out at {pct:.0}% of the source, over max_encoded_percent", w.stem);
        }
    }

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

pub fn handle_failure(job: &Job, ctx: &JobContext, stem: &str, err: &anyhow::Error, shutting_down: bool) {
    let temp = TempDir::for_video(&job.output_dir(&ctx.output_dir), stem);
    // A source replaced or gone mid-job is no verdict on the file now in its place.
    let source_moved = temp.recorded_id().is_some() && !temp.source_unchanged(&job.source_file);

    // Still loud - a typo in encode.toml has to be seen - but not a verdict on the file.
    if shutting_down || source_moved || is_transient(err) {
        tracing::error!("[{stem}] job failed - retrying on the next scan\n{err:#}");
        return;
    }

    if let Err(e) = temp.create_dirs() {
        tracing::warn!("[{stem}] could not create temp dir for failure marker: {e:#}");
    }
    // So the marker locks out this file, not the next one with the same name.
    if temp.recorded_id().is_none()
        && let Err(e) = crate::resume::write_atomic(
            &temp.source_id_path, crate::resume::source_id(&job.source_file).as_bytes(),
        )
    {
        tracing::warn!("[{stem}] could not record source path: {e:#}");
    }
    if let Err(e) = crate::resume::write_atomic(&temp.failed_path, format!("{err:#}").as_bytes()) {
        tracing::warn!("[{stem}] could not write failure marker: {e:#}");
    }
    tracing::error!("[{stem}] job failed - source kept, temp dir preserved\n{err:#}");
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
    let source_shift_ms = -probe_source_video(&job.source_file)?.matroska_start_ms;
    // mkvmerge times the B-frames of an AVI no better than FFMS2 orders them.
    let video_file = frame_accurate_source(&job.source_file, temp, stem)?;
    let video = MuxVideo {
        path: &video_file, args: Vec::new(), timestamps: None, source_shift_ms,
        remove: false, expected_frames: None,
    };
    finalize(job, ctx, config, temp, &audio_plan, video)
}

fn ignored_video_opts(a: &crate::config::AvetConfig) -> Vec<&'static str> {
    let mut v = Vec::new();
    if a.dv { v.push("dv"); }
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
    deinterlace: Option<crate::interlace::FieldOrder>,
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
    })
    // SVT-AV1 drops an odd last column or row itself; doing it here keeps every stage aligned.
    .or_else(|| {
        let even = Crop { w: src_w & !1, h: src_h & !1, x: 0, y: 0 };
        ((even.w, even.h) != (src_w, src_h)).then(|| {
            tracing::info!("[{stem}] odd frame size {src_w}x{src_h}: encoding {}x{}", even.w, even.h);
            even
        })
    });

    // Interlaced 4:2:0 alternates fields by chroma row, so a crop from row 2 would swap them.
    let src_crop = match (src_crop, deinterlace) {
        (Some(c), Some(_)) if c.y % 4 != 0 => Some(Crop { y: c.y - c.y % 4, h: c.h + c.y % 4, ..c }),
        (c, _) => c,
    };

    let (eff_w, eff_h) = match src_crop {
        Some(c) => (c.w, c.h),
        None    => (src_w, src_h),
    };

    let scale_target: Option<(u32, u32)> = target_height.filter(|&th| eff_h > th).map(|th| {
        let th = round_down_even(th);
        let tw = (u64::from(eff_w) * u64::from(th) + u64::from(eff_h) / 2) / u64::from(eff_h);
        let tw = round_down_even(tw as u32);
        tracing::info!("[{stem}] auto-scale: {eff_w}x{eff_h} to {tw}x{th}");
        (tw, th)
    });

    let scene_vf = build_scene_vf(src_crop, deinterlace, scale_target);

    (scale_target, src_crop, scene_vf)
}

/// ffmpeg -vf filter for scene detection, in the order the encode pipe applies them.
fn build_scene_vf(
    crop: Option<Crop>,
    deinterlace: Option<crate::interlace::FieldOrder>,
    scale_target: Option<(u32, u32)>,
) -> Option<String> {
    let filters: Vec<String> = [
        crop.map(|c| c.to_filter()),
        deinterlace.map(|d| d.filter().to_string()),
        scale_target.map(|(w, h)| format!("scale={w}:{h}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!filters.is_empty()).then(|| filters.join(","))
}

fn round_down_even(v: u32) -> u32 {
    v & !1
}

/// The args without one `--flag value` pair.
fn without_arg(args: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip = false;
    for (i, a) in args.iter().enumerate() {
        if skip {
            skip = false;
            continue;
        }
        if a == flag && i + 1 < args.len() {
            skip = true;
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// Stable hash of everything that affects chunk output and scene boundaries.
fn profile_fingerprint(
    encoder: Option<crate::config::Encoder>,
    merged_args: &[String],
    opts: &EncodeOptions,
    scene_cfg: &crate::config::SceneDetectionConfig,
    tq: Option<&TargetQualityConfig>,
) -> String {
    let mut parts = vec![
        format!("{encoder:?}"),
        merged_args.join(" "),
        format!("{:?}", opts.scale),
        format!("{:?}", opts.crop),
        format!("{:?}", opts.target_bit_depth),
        format!("{scene_cfg:?}"),
        format!("{tq:?}"),
    ];
    if let Some(order) = opts.deinterlace {
        parts.push(format!("deinterlace={order:?}"));
    }
    if opts.dynamic_hdr.any() {
        parts.push(format!("{:?}", opts.dynamic_hdr));
        // The bitstream values and the decoder's fallback are not the same metadata.
        parts.push(format!("hdr10plus_bitstream={}", opts.hdr10plus_frames.is_some()));
    }
    format!("{:016x}", crate::resume::stable_hash(&parts.join("|")))
}

/// A probe killed mid-run leaves its encode behind, and nothing else in the temp dir's
/// housekeeping knows those names.
fn sweep_probe_leftovers(dir: &Path, stem: &str) {
    const PREFIXES: [&str; 3] = ["probe_", "cvvdp_", "cambi_"];

    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if PREFIXES.iter().any(|p| name.starts_with(p)) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!("[{stem}] removed {removed} leftover probe file(s)");
    }
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
    crate::resume::write_atomic(&temp.fingerprint_path, fingerprint.as_bytes())
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

    // Two in a row, not one: rsync's delta pass and a stalled download both hold the size
    // still for longer than a single interval.
    const STABLE_SAMPLES: u32 = 2;

    let mut state = file_state(path);
    if state.0 == 0 {
        return Err(missing());
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(TIMEOUT_SECS);
    let mut announced = false;
    let mut unchanged = 0;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(INTERVAL_SECS));
        let next = file_state(path);
        // Moved away mid-wait it reads as (0, None) twice, which compares as stable.
        if next.0 == 0 {
            return Err(missing());
        }
        if next == state {
            unchanged += 1;
            if unchanged >= STABLE_SAMPLES {
                return Ok(());
            }
        } else {
            unchanged = 0;
            if !announced {
                tracing::info!("[{stem}] file is still being written - waiting...");
                announced = true;
            }
            state = next;
        }

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

/// Cumulative source bytes by frame; empty when it cannot be trusted, which disables the
/// size cap for the whole job rather than silently for its tail.
fn probe_source_byte_index(source: &Path, frames: u64, stem: &str) -> Result<Vec<u64>> {
    let parsed: Packets = match crate::ext::ffprobe_json_with_timeout(
        &["-v", "error", "-select_streams", "v:0",
          "-show_entries", "packet=size,pts", "-of", "json"],
        source,
        crate::ext::whole_file_timeout(source, 3600),
    ) {
        Ok(p) => p,
        Err(e) if is_transient(&e) => return Err(e).context("probe the source's packet sizes"),
        Err(e) => {
            tracing::warn!("[{stem}] source packet-size probe failed: {e:#} - size cap disabled");
            return Ok(Vec::new());
        }
    };

    if parsed.packets.len() as u64 != frames {
        tracing::warn!(
            "[{stem}] the source has {} video packets but {frames} frames - size cap disabled",
            parsed.packets.len()
        );
        return Ok(Vec::new());
    }

    match cumulative_packet_bytes(&parsed.packets) {
        Some(cum) => Ok(cum),
        None => {
            tracing::warn!("[{stem}] only some of the source's video packets are timestamped - size cap disabled");
            Ok(Vec::new())
        }
    }
}

#[derive(serde::Deserialize)]
struct Packets { #[serde(default)] packets: Vec<Pkt> }
#[derive(serde::Deserialize)]
struct Pkt { #[serde(default)] size: Option<String>, #[serde(default)] pts: Option<i64> }

/// Decode order in, presentation order out. None where only part of the stream is
/// timestamped: sorting would move those few packets in front of everything.
fn cumulative_packet_bytes(packets: &[Pkt]) -> Option<Vec<u64>> {
    let timestamped = packets.iter().filter(|p| p.pts.is_some()).count();
    if timestamped != 0 && timestamped != packets.len() {
        return None;
    }

    let mut sizes: Vec<(i64, u64)> = packets
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            let size = pk.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);
            (pk.pts.unwrap_or(i as i64), size)
        })
        .collect();
    sizes.sort_by_key(|&(pts, _)| pts);

    let mut cum = Vec::with_capacity(sizes.len() + 1);
    let mut acc = 0u64;
    cum.push(0);
    for &(_, size) in &sizes {
        acc += size;
        cum.push(acc);
    }
    Some(cum)
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

fn write_timestamps(path: &Path, ts: &[f64], offset_ms: i64) -> Result<()> {
    use std::fmt::Write;
    let mut text = String::from("# timestamp format v2\n");
    for t in ts {
        let _ = writeln!(text, "{:.3}", t + offset_ms as f64);
    }
    crate::resume::write_atomic(path, text.as_bytes())
}

/// What the video stream needs beyond its frames to play back where and how it did.
#[derive(Debug, PartialEq)]
struct SourceVideo {
    fps_num: u32,
    fps_den: u32,
    /// First frame after the container start, which ffmpeg rebases the audio to.
    offset_ms: i64,
    /// mkvmerge reads a Matroska source's own tracks and chapters without that rebase.
    matroska_start_ms: i64,
    sar: Option<(u32, u32)>,
    /// Degrees counter-clockwise, as ffprobe reports the display matrix.
    rotation: i64,
}

impl SourceVideo {
    fn display_args(&self, width: u32, height: u32) -> Vec<String> {
        let mut args = Vec::new();
        if let Some((n, d)) = self.sar.filter(|&(n, d)| n > 0 && d > 0 && n != d) {
            let display_width = (u64::from(width) * u64::from(n) + u64::from(d) / 2) / u64::from(d);
            args.extend(["--display-dimensions".into(), format!("0:{display_width}x{height}")]);
        }
        if self.rotation != 0 {
            args.extend(["--projection-pose-roll".into(), format!("0:{}", self.rotation)]);
        }
        args
    }
}

#[derive(serde::Deserialize)]
struct VideoProbe { streams: Vec<VideoProbeStream>, #[serde(default)] format: VideoProbeFormat }
#[derive(serde::Deserialize)]
struct VideoProbeStream {
    avg_frame_rate: String,
    #[serde(default)]
    r_frame_rate: String,
    start_time: Option<String>,
    sample_aspect_ratio: Option<String>,
    #[serde(default)]
    side_data_list: Vec<VideoProbeSideData>,
}
#[derive(serde::Deserialize)]
struct VideoProbeSideData { rotation: Option<f64> }
#[derive(serde::Deserialize, Default)]
struct VideoProbeFormat { #[serde(default)] format_name: String, start_time: Option<String> }

fn probe_source_video(source: &Path) -> Result<SourceVideo> {
    let probe: VideoProbe = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "v:0",
          "-show_entries", "stream=avg_frame_rate,r_frame_rate,start_time,sample_aspect_ratio",
          "-show_entries", "stream_side_data=rotation",
          "-show_entries", "format=format_name,start_time",
          "-of", "json"],
        source,
    )
    .context("probe the source video stream")?;
    SourceVideo::from_probe(probe)
}

impl SourceVideo {
    fn from_probe(probe: VideoProbe) -> Result<Self> {
        let stream = probe.streams.into_iter().next().context("ffprobe found no video stream")?;
        // ffprobe leaves avg_frame_rate at 0/0 where it cannot average, e.g. a one-picture
        // MPEG-TS; r_frame_rate is the container's own rate and still there.
        let (fps_num, fps_den) = parse_fps(&stream.avg_frame_rate)
            .or_else(|e| parse_fps(&stream.r_frame_rate).map_err(|_| e))
            .context("ffprobe reported no usable frame rate for the source")?;

        let ms = |t: Option<&str>| t.and_then(|t| t.parse::<f64>().ok()).map(|s| (s * 1000.0).round() as i64);
        let container_start = ms(probe.format.start_time.as_deref()).unwrap_or(0);
        let video_start = ms(stream.start_time.as_deref()).unwrap_or(container_start);

        let sar = stream.sample_aspect_ratio.as_deref()
            .and_then(|r| r.split_once(':'))
            .and_then(|(n, d)| Some((n.parse().ok()?, d.parse().ok()?)));
        let rotation = stream.side_data_list.iter()
            .find_map(|s| s.rotation)
            .map(|r| (r.round() as i64).rem_euclid(360))
            .map(|r| if r > 180 { r - 360 } else { r })
            .unwrap_or(0);

        Ok(SourceVideo {
            fps_num,
            fps_den,
            offset_ms: (video_start - container_start).max(0),
            matroska_start_ms: if probe.format.format_name.contains("matroska") { container_start } else { 0 },
            sar,
            rotation,
        })
    }
}

fn parse_fps(rate: &str) -> Result<(u32, u32)> {
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

        let mut o = opts();
        o.dynamic_hdr = DynamicHdr { hdr10plus: true, dolby_vision: false };
        let hdr10plus = profile_fingerprint(enc, &args, &o, &sc, None);
        assert_ne!(base, hdr10plus);
        o.dynamic_hdr.dolby_vision = true;
        assert_ne!(hdr10plus, profile_fingerprint(enc, &args, &o, &sc, None));

        // Falling back to the decoder's values must not resume onto bitstream chunks.
        let mut o = opts();
        o.dynamic_hdr = DynamicHdr { hdr10plus: true, dolby_vision: false };
        o.hdr10plus_frames = Some(crate::hevc::Hdr10PlusFrames::default());
        assert_ne!(hdr10plus, profile_fingerprint(enc, &args, &o, &sc, None));
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
        assert!(format!("{err:#}").contains("avet.scale"), "got: {err:#}");
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

    fn packets(json: &str) -> Vec<Pkt> {
        serde_json::from_str::<Packets>(json).unwrap().packets
    }

    #[test]
    fn the_size_cap_gives_up_on_a_partly_timestamped_source() {
        let ordered = packets(
            r#"{"packets": [{"size": "100", "pts": 3}, {"size": "10", "pts": 1}, {"size": "1", "pts": 2}]}"#,
        );
        assert_eq!(cumulative_packet_bytes(&ordered).unwrap(), [0, 10, 11, 111]);

        // No timestamps anywhere: decode order is all there is, and it is kept.
        let none = packets(r#"{"packets": [{"size": "100"}, {"size": "10"}]}"#);
        assert_eq!(cumulative_packet_bytes(&none).unwrap(), [0, 100, 110]);

        // Mixed: sorting would move the untimestamped packets to the front.
        let mixed = packets(r#"{"packets": [{"size": "100"}, {"size": "10", "pts": 90000}]}"#);
        assert_eq!(cumulative_packet_bytes(&mixed), None);
    }

    #[test]
    fn crop_is_normalized_before_anything_downstream_sees_it() {
        // Unrounded, the metric tool and the encoder compared frames a line apart.
        let (scale, crop, vf) = compute_output_params(1920, 1080, Some("crop=1920:801:0:141"), None, None, "t");
        let crop = crop.expect("crop should survive normalization");
        assert_eq!((crop.w, crop.h, crop.x, crop.y), (1920, 800, 0, 140));
        assert_eq!(vf.as_deref(), Some("crop=1920:800:0:140"));
        assert_eq!(scale, None);
    }

    fn source_video(json: &str) -> SourceVideo {
        SourceVideo::from_probe(serde_json::from_str(json).unwrap()).unwrap()
    }

    #[test]
    fn stream_offsets_are_measured_from_the_container_start() {
        let ts = source_video(r#"{"streams": [{"avg_frame_rate": "25/1", "start_time": "1.721000"}],
            "format": {"format_name": "mpegts", "start_time": "1.400000"}}"#);
        assert_eq!((ts.offset_ms, ts.matroska_start_ms), (321, 0));

        // mkvmerge keeps a Matroska source's own timeline, so its start has to be undone.
        let mkv = source_video(r#"{"streams": [{"avg_frame_rate": "24/1", "start_time": "5.300000"}],
            "format": {"format_name": "matroska,webm", "start_time": "5.000000"}}"#);
        assert_eq!((mkv.offset_ms, mkv.matroska_start_ms), (300, 5000));

        let bare = source_video(r#"{"streams": [{"avg_frame_rate": "24000/1001"}]}"#);
        assert_eq!((bare.fps_num, bare.fps_den, bare.offset_ms, bare.matroska_start_ms), (24000, 1001, 0, 0));
    }

    #[test]
    fn aspect_and_rotation_become_mkvmerge_options() {
        let anamorphic = source_video(r#"{"streams": [{"avg_frame_rate": "25/1", "sample_aspect_ratio": "64:45"}]}"#);
        assert_eq!(anamorphic.display_args(720, 576), ["--display-dimensions", "0:1024x576"]);
        assert_eq!(anamorphic.display_args(360, 288), ["--display-dimensions", "0:512x288"]);

        let square = source_video(r#"{"streams": [{"avg_frame_rate": "25/1", "sample_aspect_ratio": "1:1"}]}"#);
        assert!(square.display_args(1920, 1080).is_empty());
        let unknown = source_video(r#"{"streams": [{"avg_frame_rate": "25/1", "sample_aspect_ratio": "0:1"}]}"#);
        assert!(unknown.display_args(1920, 1080).is_empty());

        let rotated = |r: &str| source_video(&format!(
            r#"{{"streams": [{{"avg_frame_rate": "30/1", "side_data_list": [{{"rotation": {r}}}]}}]}}"#
        )).rotation;
        assert_eq!([rotated("90"), rotated("-90"), rotated("270"), rotated("180"), rotated("-180"), rotated("0")],
                   [90, -90, -90, 180, 180, 0]);
        assert_eq!(source_video(r#"{"streams": [{"avg_frame_rate": "30/1", "side_data_list": [{"rotation": 90}]}]}"#)
            .display_args(640, 360), ["--projection-pose-roll", "0:90"]);
    }

    #[test]
    fn an_odd_frame_size_loses_its_last_column_and_row_up_front() {
        let (scale, crop, vf) = compute_output_params(321, 181, None, None, None, "t");
        assert_eq!(crop, Some(Crop { w: 320, h: 180, x: 0, y: 0 }));
        assert_eq!(vf.as_deref(), Some("crop=320:180:0:0"));
        assert_eq!(scale, None);

        assert_eq!(compute_output_params(320, 180, None, None, None, "t").1, None);
    }

    #[test]
    fn crop_larger_than_the_source_is_dropped() {
        let (_, crop, vf) = compute_output_params(1280, 720, Some("crop=1920:800:0:140"), None, None, "t");
        assert_eq!(crop, None);
        assert_eq!(vf, None);
    }

    #[test]
    fn scale_applies_to_the_cropped_size_and_keeps_even_edges() {
        let (scale, crop, vf) = compute_output_params(1920, 1080, Some("crop=1920:800:0:140"), Some(400), None, "t");
        assert_eq!(crop.map(|c| (c.w, c.h)), Some((1920, 800)));
        assert_eq!(scale, Some((960, 400)));
        assert_eq!(vf.as_deref(), Some("crop=1920:800:0:140,scale=960:400"));

        let tff = Some(crate::interlace::FieldOrder::Tff);
        let (_, _, vf) = compute_output_params(720, 576, Some("crop=704:576:8:0"), Some(288), tff, "t");
        assert_eq!(vf.as_deref(), Some("crop=704:576:8:0,bwdif=mode=send_frame:parity=tff:deint=all,scale=352:288"));
    }

    #[test]
    fn an_interlaced_crop_starts_on_a_whole_pair_of_chroma_rows() {
        let tff = Some(crate::interlace::FieldOrder::Tff);
        let crop = |y, h, deinterlace| {
            compute_output_params(720, 576, Some(&format!("crop=720:{h}:0:{y}")), None, deinterlace, "t").1.map(|c| (c.y, c.h))
        };
        assert_eq!(crop(70, 436, tff), Some((68, 438)));
        assert_eq!(crop(72, 432, tff), Some((72, 432)));
        assert_eq!(crop(70, 436, None), Some((70, 436)));
    }

    #[test]
    fn leading_frames_join_the_first_chunk_and_every_cut_moves() {
        let mut scenes = vec![scene(0, 0, 24), scene(1, 25, 99), scene(2, 100, 324)];
        shift_scenes(&mut scenes, 6);
        let bounds: Vec<(u64, u64)> = scenes.iter().map(|s| (s.start_frame, s.end_frame)).collect();
        assert_eq!(bounds, [(0, 30), (31, 105), (106, 330)]);
    }

    #[test]
    fn scale_above_the_source_height_is_not_an_upscale() {
        let (scale, _, vf) = compute_output_params(1280, 720, None, Some(1080), None, "t");
        assert_eq!(scale, None);
        assert_eq!(vf, None);
    }

    #[test]
    fn a_scale_reaches_the_target_height_exactly() {
        assert_eq!(compute_output_params(3840, 2140, None, Some(1080), None, "t").0, Some((1938, 1080)));
        assert_eq!(compute_output_params(1920, 804, None, Some(480), None, "t").0, Some((1146, 480)));
        assert_eq!(compute_output_params(320, 138, None, Some(88), None, "t").0, Some((204, 88)));
        assert_eq!(compute_output_params(1920, 1080, None, Some(721), None, "t").0, Some((1280, 720)));
        for h in (66..=4320).step_by(2) {
            assert_eq!(compute_output_params(3840, h, None, Some(64), None, "t").0.map(|s| s.1), Some(64), "height {h}");
        }
    }

    fn scene(index: usize, start: u64, end: u64) -> SceneEntry {
        SceneEntry { index, start_frame: start, end_frame: end }
    }

    #[test]
    fn a_chunk_list_with_a_gap_is_refused_instead_of_encoded_short() {
        assert!(validate_scene_list(&[scene(0, 0, 99), scene(1, 100, 239)], 240).is_ok());

        let gap = validate_scene_list(&[scene(0, 0, 99), scene(1, 120, 239)], 240).unwrap_err();
        assert!(format!("{gap:#}").contains("gap or an overlap"), "got: {gap:#}");

        let overlap = validate_scene_list(&[scene(0, 0, 99), scene(1, 90, 239)], 240).unwrap_err();
        assert!(format!("{overlap:#}").contains("gap or an overlap"), "got: {overlap:#}");

        assert!(validate_scene_list(&[scene(0, 1, 239)], 240).is_err());
        assert!(validate_scene_list(&[scene(0, 0, 238)], 240).is_err());
        assert!(validate_scene_list(&[scene(0, 0, 240)], 240).is_err());
        assert!(validate_scene_list(&[], 240).is_err());
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
    fn a_marker_is_written_only_for_a_job_that_really_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("input").join("p");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("film.mkv"), b"data").unwrap();

        let ctx = JobContext { input_dir: dir.path().join("input"), output_dir: dir.path().join("output") };
        let job = Job {
            encode_toml: profile.join("encode.toml"),
            source_file: profile.join("film.mkv"),
            rel_dir: PathBuf::new(),
        };
        let marker = TempDir::for_video(&job.output_dir(&ctx.output_dir), "film").failed_path;

        // A stop mid-job is no verdict on the file.
        let err = anyhow::anyhow!("encoder exited with status 1");
        handle_failure(&job, &ctx, "film", &err, true);
        assert!(!marker.exists(), "a shutdown wrote a failure marker");

        handle_failure(&job, &ctx, "film", &anyhow::Error::new(Transient).context("ffprobe timed out"), false);
        assert!(!marker.exists(), "a transient failure wrote a failure marker");

        handle_failure(&job, &ctx, "film", &err, false);
        assert!(marker.exists(), "a real failure wrote no marker");
        assert!(std::fs::read_to_string(&marker).unwrap().contains("status 1"));
    }

    #[test]
    fn a_source_replaced_mid_job_is_retried_and_not_locked_out() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("input").join("p");
        std::fs::create_dir_all(&profile).unwrap();
        let source = profile.join("film.mkv");
        std::fs::write(&source, b"first").unwrap();

        let ctx = JobContext { input_dir: dir.path().join("input"), output_dir: dir.path().join("output") };
        let job = Job { encode_toml: profile.join("encode.toml"), source_file: source.clone(), rel_dir: PathBuf::new() };
        let temp = TempDir::for_video(&job.output_dir(&ctx.output_dir), "film");
        temp.claim_source(&source, "film").unwrap();
        let claimed = temp.recorded_id();

        std::fs::write(&source, b"a longer replacement").unwrap();
        handle_failure(&job, &ctx, "film", &anyhow::anyhow!("the index does not match the source file"), false);
        assert!(!temp.failed_path.exists(), "the replacement was locked out");
        assert_eq!(temp.recorded_id(), claimed);
    }

    #[test]
    fn a_timeout_and_a_full_disk_are_transient_but_an_encoder_failure_is_not() {
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
