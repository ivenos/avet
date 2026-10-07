use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Missing or empty reads as the default; the fingerprint would never clear a leftover.
fn load_json_or_default<T: DeserializeOwned + Default>(path: &Path, what: &str) -> Result<T> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(e).with_context(|| format!("read {what}: {}", path.display())),
    };
    if raw.trim().is_empty() {
        tracing::warn!("{} is empty - starting {what} over", path.display());
        return Ok(T::default());
    }
    serde_json::from_str(&raw).with_context(|| format!("parse {what}: {}", path.display()))
}

/// Temp file + rename, flushed first, or a power loss leaves zero bytes behind.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("flush {} to disk", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
}

pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, serde_json::to_string_pretty(value)?.as_bytes())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SceneEntry {
    pub index: usize,
    pub start_frame: u64,
    pub end_frame: u64,
}

impl SceneEntry {
    pub fn frame_count(&self) -> u64 {
        self.end_frame - self.start_frame + 1
    }

    pub fn padded_index(&self) -> String {
        format!("{:05}", self.index + 1)
    }
}

pub fn read_scenes(path: &Path) -> Result<Vec<SceneEntry>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read scenes.json: {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parse scenes.json: {}", path.display()))
}

pub fn write_scenes(path: &Path, scenes: &[SceneEntry]) -> Result<()> {
    write_json_atomic(path, &scenes)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChunkInfo {
    pub frames: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct DoneState {
    pub chunks: HashMap<String, ChunkInfo>,
}

pub struct DoneFile {
    pub path: PathBuf,
    pub state: Mutex<DoneState>,
}

impl DoneFile {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let state = load_json_or_default(path, "done.json")?;
        Ok(Self { path: path.to_owned(), state: Mutex::new(state) })
    }

    /// `frames` too: scenes.json can be re-detected with other boundaries while done.json
    /// survives, and then the chunk holds the wrong range at a coincidentally equal size.
    pub fn is_done(&self, chunk_key: &str, chunk_path: &Path, frames: u64) -> bool {
        let expected = match self.state.lock().unwrap().chunks.get(chunk_key) {
            Some(info) if info.frames == frames => info.size_bytes,
            _ => return false,
        };
        matches!(std::fs::metadata(chunk_path), Ok(m) if m.len() == expected && expected > 0)
    }

    pub fn mark_done(&self, chunk_key: &str, frames: u64, size_bytes: u64) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.chunks.insert(chunk_key.to_owned(), ChunkInfo { frames, size_bytes });
        write_json_atomic(&self.path, &*state)
    }
}

/// A bare number: solved before the score was kept, or scored NaN, which serde_json cannot read back.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(untagged)]
enum Solved {
    Scored {
        crf: f64,
        jod: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        measured: Option<f64>,
    },
    Crf(f64),
}

/// Per-chunk solved CRF cache for target quality, so a resume skips re-probing.
pub struct CrfCache {
    pub path: PathBuf,
    state: Mutex<HashMap<String, Solved>>,
}

impl CrfCache {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let state = load_json_or_default(path, "tq.json")?;
        Ok(Self { path: path.to_owned(), state: Mutex::new(state) })
    }

    pub fn get(&self, chunk_key: &str) -> Option<f64> {
        self.state.lock().unwrap().get(chunk_key).map(|s| match *s {
            Solved::Scored { crf, .. } | Solved::Crf(crf) => crf,
        })
    }

    /// Of the finished chunk where that was measured, else of the probe.
    pub fn jod(&self, chunk_key: &str) -> Option<f64> {
        match self.state.lock().unwrap().get(chunk_key) {
            Some(Solved::Scored { jod, measured, .. }) => Some(measured.unwrap_or(*jod)),
            _ => None,
        }
    }

    pub fn probed(&self, chunk_key: &str) -> Option<f64> {
        match self.state.lock().unwrap().get(chunk_key) {
            Some(Solved::Scored { jod, .. }) => Some(*jod),
            _ => None,
        }
    }

    pub fn insert(&self, chunk_key: &str, crf: f64, jod: f64) -> Result<()> {
        let solved = if jod.is_finite() { Solved::Scored { crf, jod, measured: None } } else { Solved::Crf(crf) };
        let mut state = self.state.lock().unwrap();
        state.insert(chunk_key.to_owned(), solved);
        write_json_atomic(&self.path, &*state)
    }

    pub fn set_measured(&self, chunk_key: &str, jod: f64) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(Solved::Scored { measured, .. }) = state.get_mut(chunk_key) {
            *measured = Some(jod);
        }
        write_json_atomic(&self.path, &*state)
    }
}

/// FNV-1a, not `DefaultHasher`: that one is explicitly free to change between Rust
/// releases, and a new value here discards every chunk of every job in flight.
pub fn stable_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// `.avet_<stem>`, shortened with a hash of the stem where that passes the 255-byte name limit.
fn temp_dir_name(stem: &str) -> String {
    const NAME_MAX: usize = 255;

    let name = format!(".avet_{stem}");
    if name.len() <= NAME_MAX {
        return name;
    }
    let suffix = format!("-{:016x}", stable_hash(stem));
    let mut end = NAME_MAX - suffix.len();
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &name[..end])
}

pub struct TempDir {
    pub path: PathBuf,
    pub index_path: PathBuf,
    pub scenes_path: PathBuf,
    pub done_path: PathBuf,
    pub tq_path: PathBuf,
    pub fingerprint_path: PathBuf,
    pub source_id_path: PathBuf,
    pub failed_path: PathBuf,
    pub failed_profile_path: PathBuf,
    pub delivered_path: PathBuf,
    pub attempts_path: PathBuf,
    pub chunks_dir: PathBuf,
    pub crop_cache: PathBuf,
    pub interlace_cache: PathBuf,
    pub tracks_path: PathBuf,
    pub remux_path: PathBuf,
    pub video_path: PathBuf,
    pub mux_path: PathBuf,
    pub timestamps_path: PathBuf,
    pub hdr10plus_path: PathBuf,
}

impl TempDir {
    pub fn for_video(output_dir: &Path, video_stem: &str) -> Self {
        let path = output_dir.join(temp_dir_name(video_stem));
        let index_path       = path.join("frame-index.ffindex");
        let scenes_path      = path.join("scenes.json");
        let done_path        = path.join("done.json");
        let tq_path          = path.join("tq.json");
        let fingerprint_path = path.join("profile.fingerprint");
        let source_id_path   = path.join("source.path");
        let failed_path      = path.join(".failed");
        let failed_profile_path = path.join("failed.profile");
        let delivered_path   = path.join("delivered");
        let attempts_path    = path.join("attempts");
        let chunks_dir       = path.join("chunks");
        let crop_cache       = path.join("crop.cache");
        let interlace_cache  = path.join("interlace.cache");
        let tracks_path      = path.join("tracks.mkv");
        let remux_path       = path.join("source.mkv");
        let video_path       = path.join("video.ivf");
        let mux_path         = path.join("muxed.mkv");
        let timestamps_path  = path.join("timestamps.txt");
        let hdr10plus_path   = path.join("hdr10plus.json");
        Self {
            path, index_path, scenes_path, done_path, tq_path,
            fingerprint_path, source_id_path, failed_path, failed_profile_path, delivered_path, attempts_path,
            chunks_dir, crop_cache, interlace_cache,
            tracks_path, remux_path, video_path, mux_path, timestamps_path, hdr10plus_path,
        }
    }

    pub fn create_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.chunks_dir)
            .with_context(|| format!("create {}", self.chunks_dir.display()))
    }

    pub fn chunk_path(&self, key: &str) -> PathBuf {
        self.chunks_dir.join(format!("{key}.ivf"))
    }

    /// The source this temp dir was built for, as recorded by `claim_source`.
    pub fn recorded_id(&self) -> Option<String> {
        std::fs::read_to_string(&self.source_id_path)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// A different source with the same stem wipes the dir; it describes the old video.
    pub fn claim_source(&self, source: &Path, stem: &str) -> Result<()> {
        let id = source_id(source)?;
        let recorded = self.recorded_id();
        if recorded.as_ref().is_some_and(|prev| *prev != id) {
            tracing::warn!("[{stem}] temp dir belongs to a different source - discarding it");
            std::fs::remove_dir_all(&self.path)
                .with_context(|| format!("remove stale temp dir: {}", self.path.display()))?;
        }
        self.create_dirs()?;
        if recorded.as_ref() == Some(&id) {
            return Ok(());
        }
        write_atomic(&self.source_id_path, id.as_bytes())
    }

    pub fn source_unchanged(&self, source: &Path) -> bool {
        self.recorded_id().is_some_and(|id| source_id(source).is_ok_and(|now| now == id))
    }

    /// A marker is a verdict on the source under one encode.toml; a marker without a record stays.
    pub fn failed_under_another_profile(&self, encode_toml: &Path) -> bool {
        let then = std::fs::read_to_string(&self.failed_profile_path).ok();
        match (then, profile_id(encode_toml)) {
            (Some(then), Some(now)) => then.trim() != now,
            _ => false,
        }
    }

    pub fn clear_failed(&self) {
        let _ = std::fs::remove_file(&self.failed_path);
        let _ = std::fs::remove_file(&self.failed_profile_path);
    }

    /// The output of this very source is in place and the source not yet archived.
    pub fn awaits_archiving(&self, source: &Path) -> bool {
        self.delivered_path.exists() && self.source_unchanged(source)
    }

    pub fn mark_delivered(&self, keep_temp: bool) -> Result<()> {
        write_atomic(&self.delivered_path, if keep_temp { b"keep" } else { b"" })
    }

    /// From here on the temp dir only says that the source still has to be archived.
    pub fn clear_work(&self) -> Result<()> {
        for entry in std::fs::read_dir(&self.path).with_context(|| format!("read {}", self.path.display()))? {
            let entry = entry.with_context(|| format!("read {}", self.path.display()))?;
            let path = entry.path();
            if path == self.delivered_path || path == self.source_id_path {
                continue;
            }
            let removed = if entry.file_type().is_ok_and(|t| t.is_dir()) {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            removed.with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(())
    }

    pub fn finish_delivery(&self) -> Result<()> {
        if std::fs::read(&self.delivered_path).is_ok_and(|kept| kept == b"keep") {
            return std::fs::remove_file(&self.delivered_path)
                .with_context(|| format!("remove {}", self.delivered_path.display()));
        }
        std::fs::remove_dir_all(&self.path).with_context(|| format!("remove {}", self.path.display()))
    }
}

pub fn profile_id(encode_toml: &Path) -> Option<String> {
    let raw = std::fs::read(encode_toml).ok()?;
    Some(format!("{:016x}", stable_hash(&String::from_utf8_lossy(&raw))))
}

/// The size below the path, so a file replaced under the same name is a new job. Not the
/// mtime: a copy that only touched it would throw away hours of encoding.
pub fn source_id(source: &Path) -> Result<String> {
    let size = std::fs::metadata(source).with_context(|| format!("read the size of {}", source.display()))?.len();
    Ok(format!("{}\n{size}", source.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenes(n: usize) -> Vec<SceneEntry> {
        (0..n)
            .map(|i| SceneEntry {
                index: i,
                start_frame: i as u64 * 10,
                end_frame: i as u64 * 10 + 9,
            })
            .collect()
    }

    #[test]
    fn a_shorter_rewrite_leaves_no_tail_and_no_temp_file() {
        // In place, the second write would leave the tail of the first behind.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("scenes.json");

        write_scenes(&path, &scenes(50)).unwrap();
        assert_eq!(read_scenes(&path).unwrap().len(), 50);

        write_scenes(&path, &scenes(1)).unwrap();
        let back = read_scenes(&path).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].end_frame, 9);

        assert!(!path.with_extension("json.tmp").exists(), "scratch file left behind");
    }

    #[test]
    fn a_source_replaced_under_the_same_name_discards_the_temp_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("Film.mkv");
        std::fs::write(&source, b"first video").unwrap();

        let temp = TempDir::for_video(dir.path(), "Film");
        temp.claim_source(&source, "Film").unwrap();
        let chunk = temp.chunk_path("00001");
        std::fs::write(&chunk, b"chunk of the first video").unwrap();

        temp.claim_source(&source, "Film").unwrap();
        assert!(chunk.exists(), "the same file must keep its chunks");

        std::fs::write(&source, b"a different video of another length").unwrap();
        temp.claim_source(&source, "Film").unwrap();
        assert!(!chunk.exists(), "chunks of the old video survived the replacement");
        assert_eq!(temp.recorded_id(), Some(source_id(&source).unwrap()));
    }

    #[test]
    fn a_source_that_cannot_be_read_keeps_the_temp_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("Film.mkv");
        std::fs::write(&source, b"video").unwrap();

        let temp = TempDir::for_video(dir.path(), "Film");
        temp.claim_source(&source, "Film").unwrap();
        let chunk = temp.chunk_path("00001");
        std::fs::write(&chunk, b"chunk").unwrap();

        // A share that drops out reads as no file at all, which is no replacement.
        std::fs::remove_file(&source).unwrap();
        assert!(temp.claim_source(&source, "Film").is_err());
        assert!(chunk.exists());
        assert!(!temp.source_unchanged(&source));
    }

    #[test]
    fn an_empty_source_record_keeps_the_chunks() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("Film.mkv");
        std::fs::write(&source, b"video").unwrap();

        let temp = TempDir::for_video(dir.path(), "Film");
        temp.claim_source(&source, "Film").unwrap();
        let chunk = temp.chunk_path("00001");
        std::fs::write(&chunk, b"chunk").unwrap();
        std::fs::write(&temp.source_id_path, b"").unwrap();

        temp.claim_source(&source, "Film").unwrap();
        assert!(chunk.exists());
        assert_eq!(temp.recorded_id(), Some(source_id(&source).unwrap()));
    }

    #[test]
    fn a_delivered_job_leaves_only_what_finishes_the_archiving() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("Film.mkv");
        std::fs::write(&source, b"video").unwrap();

        let temp = TempDir::for_video(dir.path(), "Film");
        temp.claim_source(&source, "Film").unwrap();
        std::fs::write(temp.chunk_path("00001"), b"chunk").unwrap();
        std::fs::write(&temp.index_path, b"index").unwrap();
        assert!(!temp.awaits_archiving(&source));

        temp.mark_delivered(false).unwrap();
        assert!(temp.chunk_path("00001").exists() && temp.awaits_archiving(&source));
        temp.clear_work().unwrap();
        let mut left: Vec<_> = std::fs::read_dir(&temp.path).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["delivered", "source.path"]);
        assert!(temp.awaits_archiving(&source));
        std::fs::write(&source, b"a different video").unwrap();
        assert!(!temp.awaits_archiving(&source));

        temp.finish_delivery().unwrap();
        assert!(!temp.path.exists());

        temp.claim_source(&source, "Film").unwrap();
        std::fs::write(temp.chunk_path("00001"), b"chunk").unwrap();
        temp.mark_delivered(true).unwrap();
        assert!(temp.chunk_path("00001").exists() && temp.awaits_archiving(&source));
        temp.finish_delivery().unwrap();
        assert!(temp.chunk_path("00001").exists() && !temp.delivered_path.exists());
    }

    #[test]
    fn a_long_name_still_gets_a_temp_dir_of_its_own() {
        assert_eq!(temp_dir_name("Film"), ".avet_Film");
        let long = "ü".repeat(125) + "x";
        let a = temp_dir_name(&long);
        let b = temp_dir_name(&("ü".repeat(125) + "y"));
        assert!(a.len() <= 255 && b.len() <= 255, "{} and {} bytes", a.len(), b.len());
        assert_ne!(a, b);
        assert_eq!(a, temp_dir_name(&long));

        let dir = tempfile::TempDir::new().unwrap();
        TempDir::for_video(dir.path(), &long).create_dirs().unwrap();
    }

    #[test]
    fn a_solved_crf_survives_into_the_next_run() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tq.json");

        let cache = CrfCache::load_or_create(&path).unwrap();
        assert_eq!(cache.get("00001"), None);
        cache.insert("00001", 28.25, 9.512).unwrap();
        cache.insert("00002", 31.0, f64::NAN).unwrap();

        // Each chunk costs several probe encodes and measurements to solve again.
        let reloaded = CrfCache::load_or_create(&path).unwrap();
        assert_eq!(reloaded.get("00001"), Some(28.25));
        assert_eq!(reloaded.jod("00001"), Some(9.512));
        assert_eq!((reloaded.get("00002"), reloaded.jod("00002")), (Some(31.0), None));
        assert_eq!(reloaded.get("00003"), None);

        reloaded.set_measured("00001", 9.47).unwrap();
        reloaded.set_measured("00002", 9.5).unwrap();
        let again = CrfCache::load_or_create(&path).unwrap();
        assert_eq!((again.get("00001"), again.jod("00001"), again.probed("00001")), (Some(28.25), Some(9.47), Some(9.512)));
        assert_eq!((again.get("00002"), again.jod("00002")), (Some(31.0), None));
    }

    #[test]
    fn a_tq_json_of_bare_crfs_still_resumes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tq.json");
        std::fs::write(&path, r#"{"00001": 28.25, "00002": 31.0}"#).unwrap();

        let cache = CrfCache::load_or_create(&path).unwrap();
        assert_eq!((cache.get("00001"), cache.jod("00001")), (Some(28.25), None));
        cache.insert("00003", 33.5, 9.6).unwrap();
        let reloaded = CrfCache::load_or_create(&path).unwrap();
        assert_eq!((reloaded.get("00002"), reloaded.get("00003"), reloaded.jod("00003")), (Some(31.0), Some(33.5), Some(9.6)));
    }

    #[test]
    fn a_missing_or_empty_state_file_reads_as_the_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("done.json");

        let done: DoneState = load_json_or_default(&path, "done.json").unwrap();
        assert!(done.chunks.is_empty());

        // What a crash between create and flush leaves behind.
        std::fs::write(&path, b"").unwrap();
        let done: DoneState = load_json_or_default(&path, "done.json").unwrap();
        assert!(done.chunks.is_empty());

        // Garbage is a different matter and still has to be reported.
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load_json_or_default::<DoneState>(&path, "done.json").is_err());
    }
}
