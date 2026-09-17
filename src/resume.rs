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
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    use std::io::Write;

    let json = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("flush {} to disk", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
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

    pub fn is_done(&self, chunk_key: &str, chunk_path: &Path) -> bool {
        let expected = match self.state.lock().unwrap().chunks.get(chunk_key) {
            Some(info) => info.size_bytes,
            None       => return false,
        };
        // Recorded size must match on-disk size; truncated/missing files are not "done".
        matches!(std::fs::metadata(chunk_path), Ok(m) if m.len() == expected && expected > 0)
    }

    pub fn mark_done(&self, chunk_key: &str, frames: u64, size_bytes: u64) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.chunks.insert(chunk_key.to_owned(), ChunkInfo { frames, size_bytes });
        write_json_atomic(&self.path, &*state)
    }
}

/// Per-chunk solved CRF cache for target quality, so a resume skips re-probing.
pub struct CrfCache {
    pub path: PathBuf,
    state: Mutex<HashMap<String, f64>>,
}

impl CrfCache {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let state = load_json_or_default(path, "tq.json")?;
        Ok(Self { path: path.to_owned(), state: Mutex::new(state) })
    }

    pub fn get(&self, chunk_key: &str) -> Option<f64> {
        self.state.lock().unwrap().get(chunk_key).copied()
    }

    pub fn insert(&self, chunk_key: &str, crf: f64) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.insert(chunk_key.to_owned(), crf);
        write_json_atomic(&self.path, &*state)
    }
}

/// `.avet_<stem>`, shortened with a hash of the stem where that passes the 255-byte name limit.
fn temp_dir_name(stem: &str) -> String {
    use std::hash::{Hash, Hasher};
    const NAME_MAX: usize = 255;

    let name = format!(".avet_{stem}");
    if name.len() <= NAME_MAX {
        return name;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    stem.hash(&mut h);
    let suffix = format!("-{:016x}", h.finish());
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
    pub chunks_dir: PathBuf,
    pub crop_cache: PathBuf,
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
        let chunks_dir       = path.join("chunks");
        let crop_cache       = path.join("crop.cache");
        let tracks_path      = path.join("tracks.mkv");
        let remux_path       = path.join("source.mkv");
        let video_path       = path.join("video.ivf");
        let mux_path         = path.join("muxed.mkv");
        let timestamps_path  = path.join("timestamps.txt");
        let hdr10plus_path   = path.join("hdr10plus.json");
        Self {
            path, index_path, scenes_path, done_path, tq_path,
            fingerprint_path, source_id_path, failed_path, chunks_dir, crop_cache,
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
    pub fn recorded_source(&self) -> Option<String> {
        std::fs::read_to_string(&self.source_id_path)
            .ok()
            .map(|s| s.trim().to_owned())
    }

    /// A different source with the same stem wipes the dir; it describes the old video.
    pub fn claim_source(&self, source: &Path, stem: &str) -> Result<()> {
        let id = source.display().to_string();
        if self.recorded_source().is_some_and(|prev| prev != id) {
            tracing::warn!("[{stem}] temp dir belongs to a different source - discarding it");
            std::fs::remove_dir_all(&self.path)
                .with_context(|| format!("remove stale temp dir: {}", self.path.display()))?;
        }
        self.create_dirs()?;
        std::fs::write(&self.source_id_path, &id)
            .with_context(|| format!("write {}", self.source_id_path.display()))
    }
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
