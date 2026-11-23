use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

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
    let _ = std::fs::remove_file(&tmp);
    // create_new does not follow a symlink at the name.
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))
        .and_then(|mut f| {
            f.write_all(data).with_context(|| format!("write {}", tmp.display()))?;
            f.sync_all().with_context(|| format!("flush {} to disk", tmp.display()))
        })
        .and_then(|()| std::fs::rename(&tmp, path).with_context(|| format!("rename {} to {}", tmp.display(), path.display())));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
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

    pub fn unmeasured(&self, chunk_key: &str) -> bool {
        matches!(self.state.lock().unwrap().get(chunk_key), Some(Solved::Scored { measured: None, .. }))
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

const FNV_OFFSET: u64 = 0xcbf29ce484222325;

fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// FNV-1a, because `DefaultHasher` is free to change between Rust releases.
pub fn stable_hash(text: &str) -> u64 {
    fnv1a(FNV_OFFSET, text.as_bytes())
}

/// `.avet_<stem>`, shortened with a hash of the stem where that passes the 255-byte name limit.
fn temp_dir_name(stem: &str) -> String {
    let name = format!(".avet_{stem}");
    if name.len() <= crate::scanner::NAME_MAX {
        return name;
    }
    crate::scanner::fit_name(&name, &format!("-{:016x}", stable_hash(stem)))
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
        remove_symlink(&path);
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

    fn refuse_symlink(&self) -> Result<()> {
        anyhow::ensure!(!self.path.is_symlink(), "{} is a symbolic link", self.path.display());
        Ok(())
    }

    pub fn create_dirs(&self) -> Result<()> {
        self.refuse_symlink()?;
        std::fs::create_dir_all(&self.chunks_dir)
            .with_context(|| format!("create {}", self.chunks_dir.display()))
    }

    pub fn chunk_path(&self, key: &str) -> PathBuf {
        self.chunks_dir.join(format!("{key}.ivf"))
    }

    fn record(&self) -> Option<String> {
        std::fs::read_to_string(&self.source_id_path)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub fn recorded_id(&self) -> Option<String> {
        let recorded = self.record()?;
        Some(upgraded_id(&recorded).unwrap_or(recorded))
    }

    /// A different source with the same stem wipes the dir; it describes the old video.
    pub fn claim_source(&self, source: &Path, stem: &str) -> Result<()> {
        self.refuse_symlink()?;
        let (id, read) = match source_id(source) {
            Ok(id) => (id, true),
            Err(_) => (path_and_size(source)?.0, false),
        };
        let recorded = self.record();
        let same = recorded.as_ref().is_some_and(|prev| same_source(prev, &id));
        if recorded.is_some() && !same {
            tracing::warn!("[{stem}] temp dir belongs to a different source - discarding it");
            std::fs::remove_dir_all(&self.path)
                .with_context(|| format!("remove stale temp dir: {}", self.path.display()))?;
        }
        if self.path.is_dir() {
            remove_symlinks_in(&self.path)?;
        }
        self.create_dirs()?;
        remove_symlinks_in(&self.chunks_dir)?;
        if same && (!read || recorded.as_ref() == Some(&id)) && self.source_id_path.exists() {
            return Ok(());
        }
        write_atomic(&self.source_id_path, id.as_bytes())
    }

    pub fn source_unchanged(&self, source: &Path) -> bool {
        let Some(recorded) = self.record() else { return false };
        match source_id(source) {
            Ok(now) => same_source(&recorded, &now),
            Err(_) => path_and_size(source).is_ok_and(|(now, _)| now == without_hash(&recorded)),
        }
    }

        pub fn source_replaced(&self, source: &Path) -> bool {
        let Some(recorded) = self.record() else { return false };
        scanned_id(source).is_some_and(|id| !same_source(&recorded, &id))
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
        if self.path.is_symlink() || !self.delivered_path.exists() {
            return false;
        }
        let (Some(recorded), Ok(now)) = (self.record(), source_id(source)) else { return false };
        if !same_source(&recorded, &now) {
            return false;
        }
        if recorded != now {
            let _ = write_atomic(&self.source_id_path, now.as_bytes());
        }
        true
    }

    pub fn mark_delivered(&self, keep_temp: bool) -> Result<()> {
        write_atomic(&self.delivered_path, if keep_temp { b"keep" } else { b"" })
    }

    /// From here on the temp dir only says that the source still has to be archived.
    pub fn clear_work(&self) -> Result<()> {
        self.refuse_symlink()?;
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
        self.refuse_symlink()?;
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

const ID_SAMPLE: u64 = 1 << 20;
const HASH_PREFIX: &str = "fnv1a:";

fn path_and_size(source: &Path) -> Result<(String, u64)> {
    let size = std::fs::metadata(source).with_context(|| format!("read the size of {}", source.display()))?.len();
    Ok((format!("{}\n{size}", source.display()), size))
}

fn content_hash(source: &Path, size: u64) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(source)?;
    let mut sample = Vec::new();
    (&mut file).take(ID_SAMPLE).read_to_end(&mut sample)?;
    let hash = fnv1a(FNV_OFFSET, &sample);
    if size <= ID_SAMPLE {
        return Ok(hash);
    }
    file.seek(SeekFrom::Start((size - ID_SAMPLE).max(ID_SAMPLE)))?;
    sample.clear();
    file.take(ID_SAMPLE).read_to_end(&mut sample)?;
    Ok(fnv1a(hash, &sample))
}

/// Not the mtime: a copy that only touched it would throw away hours of encoding.
pub fn source_id(source: &Path) -> Result<String> {
    let (id, size) = path_and_size(source)?;
    let hash = content_hash(source, size).with_context(|| format!("read {}", source.display()))?;
    Ok(format!("{id}\n{HASH_PREFIX}{hash:016x}"))
}

static SCANNED: Mutex<BTreeMap<PathBuf, (u64, SystemTime, String)>> = Mutex::new(BTreeMap::new());

fn scanned_id(source: &Path) -> Option<String> {
    let meta = std::fs::metadata(source).ok()?;
    let (size, modified) = (meta.len(), meta.modified().ok()?);
    let mut scanned = SCANNED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((s, m, id)) = scanned.get(source)
        && (*s, *m) == (size, modified)
    {
        return Some(id.clone());
    }
    let id = source_id(source).ok()?;
    scanned.insert(source.to_path_buf(), (size, modified, id.clone()));
    Some(id)
}

fn without_hash(id: &str) -> &str {
    id.rsplit_once('\n').filter(|(_, last)| last.starts_with(HASH_PREFIX)).map_or(id, |(rest, _)| rest)
}

/// A record from before the hash, or an id of a source that cannot be read, holds only path and size.
fn same_source(recorded: &str, id: &str) -> bool {
    recorded == id || recorded == without_hash(id) || without_hash(recorded) == id
}

fn upgraded_id(recorded: &str) -> Option<String> {
    let (path, size) = recorded.rsplit_once('\n')?;
    if size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    source_id(Path::new(path)).ok().filter(|now| same_source(recorded, now))
}

fn remove_symlink(path: &Path) {
    if !path.is_symlink() {
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => tracing::warn!("{} was a symbolic link - removed the link, its target is untouched", path.display()),
        Err(e) => crate::scanner::report(format!("could not remove the symbolic link {}: {e}", path.display())),
    }
}

fn remove_symlinks_in(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry.with_context(|| format!("read {}", dir.display()))?.path();
        remove_symlink(&path);
        anyhow::ensure!(!path.is_symlink(), "{} is a symbolic link", path.display());
    }
    Ok(())
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
    fn a_chunk_is_done_only_with_the_frames_and_the_size_it_was_finished_with() {
        let dir = tempfile::TempDir::new().unwrap();
        let chunk = dir.path().join("00001.ivf");
        std::fs::write(&chunk, b"encoded").unwrap();
        let done = DoneFile::load_or_create(&dir.path().join("done.json")).unwrap();
        assert!(!done.is_done("00001", &chunk, 24));

        done.mark_done("00001", 24, 7).unwrap();
        assert!(done.is_done("00001", &chunk, 24));
        assert!(!done.is_done("00001", &chunk, 25));
        std::fs::write(&chunk, b"cut").unwrap();
        assert!(!done.is_done("00001", &chunk, 24));
    }

    #[test]
    fn a_shorter_rewrite_leaves_no_tail_and_no_temp_file() {
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

    fn claimed_with_chunk(dir: &Path, bytes: &[u8]) -> (PathBuf, TempDir, PathBuf) {
        let source = dir.join("Film.mkv");
        std::fs::write(&source, bytes).unwrap();
        let temp = TempDir::for_video(dir, "Film");
        temp.claim_source(&source, "Film").unwrap();
        let chunk = temp.chunk_path("00001");
        std::fs::write(&chunk, b"chunk").unwrap();
        (source, temp, chunk)
    }

    #[test]
    fn only_other_bytes_at_either_end_discard_the_chunks_of_a_source_of_the_same_size() {
        let size = 3usize << 20;
        for at in [0, (1 << 20) - 1, size - (1 << 20), size - 1] {
            let dir = tempfile::TempDir::new().unwrap();
            let first = vec![7u8; size];
            let (source, temp, chunk) = claimed_with_chunk(dir.path(), &first);

            let touched = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
            std::fs::File::options().write(true).open(&source).unwrap().set_modified(touched).unwrap();
            temp.claim_source(&source, "Film").unwrap();
            assert!(chunk.exists(), "a touch discarded the chunks");

            let copy = dir.path().join("copy");
            std::fs::write(&copy, &first).unwrap();
            std::fs::rename(&copy, &source).unwrap();
            temp.claim_source(&source, "Film").unwrap();
            assert!(chunk.exists() && temp.source_unchanged(&source), "an identical copy discarded the chunks");

            let mut other = first;
            other[at] = 8;
            std::fs::write(&source, &other).unwrap();
            assert!(!temp.source_unchanged(&source), "byte {at} went unnoticed");
            temp.claim_source(&source, "Film").unwrap();
            assert!(!chunk.exists(), "chunks of the old video survived a change at byte {at}");
        }
    }

    #[test]
    fn a_small_source_is_compared_in_full() {
        for size in [5usize, (1 << 20) + 5, 2 << 20] {
            let dir = tempfile::TempDir::new().unwrap();
            let first = vec![7u8; size];
            let (source, temp, _chunk) = claimed_with_chunk(dir.path(), &first);
            let mut other = first;
            other[size - 1] = 8;
            std::fs::write(&source, &other).unwrap();
            assert!(!temp.source_unchanged(&source), "a change in a file of {size} bytes went unnoticed");
        }
    }

    #[test]
    fn a_scan_reads_a_failed_source_again_only_after_its_size_or_mtime_moved() {
        let dir = tempfile::TempDir::new().unwrap();
        let (source, temp, _) = claimed_with_chunk(dir.path(), b"video");
        let then = std::fs::metadata(&source).unwrap().modified().unwrap();
        assert!(!temp.source_replaced(&source));

        std::fs::write(&source, b"other").unwrap();
        std::fs::File::options().write(true).open(&source).unwrap().set_modified(then).unwrap();
        assert!(!temp.source_replaced(&source), "the unchanged size and mtime were read again");

        let later = then + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&source).unwrap().set_modified(later).unwrap();
        assert!(temp.source_replaced(&source));
    }

    #[test]
    fn a_record_without_a_hash_keeps_the_chunks_and_is_rewritten() {
        let dir = tempfile::TempDir::new().unwrap();
        let (source, temp, chunk) = claimed_with_chunk(dir.path(), b"video");
        let two_lines = format!("{}\n5", source.display());
        std::fs::write(&temp.source_id_path, &two_lines).unwrap();

        assert!(temp.source_unchanged(&source));
        assert_eq!(temp.recorded_id(), Some(source_id(&source).unwrap()));
        temp.claim_source(&source, "Film").unwrap();
        assert!(chunk.exists(), "a job in flight lost its chunks");
        let rewritten = std::fs::read_to_string(&temp.source_id_path).unwrap();
        assert_ne!(rewritten, two_lines);
        assert_eq!(rewritten, source_id(&source).unwrap());

        std::fs::write(&temp.source_id_path, &two_lines).unwrap();
        temp.mark_delivered(false).unwrap();
        assert!(temp.awaits_archiving(&source));
        assert_eq!(std::fs::read_to_string(&temp.source_id_path).unwrap(), source_id(&source).unwrap());
        std::fs::remove_file(&temp.delivered_path).unwrap();

        std::fs::write(&temp.source_id_path, &two_lines).unwrap();
        std::fs::write(&source, b"longer video").unwrap();
        assert!(!temp.source_unchanged(&source));
        temp.claim_source(&source, "Film").unwrap();
        assert!(!chunk.exists(), "a record of another size kept the chunks");
    }

    #[test]
    fn a_failed_marker_with_a_record_without_a_hash_still_blocks_its_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let (input, output) = (dir.path().join("input"), dir.path().join("output"));
        let profile = input.join("p");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        let source = profile.join("film.mkv");
        std::fs::write(&source, b"video").unwrap();

        let temp = TempDir::for_video(&output, "film");
        temp.create_dirs().unwrap();
        std::fs::write(&temp.failed_path, b"boom").unwrap();
        std::fs::write(&temp.source_id_path, format!("{}\n5", source.display())).unwrap();
        assert_ne!(source_id(&source).unwrap(), format!("{}\n5", source.display()));
        assert_eq!(crate::scanner::scan(&input, &output).unwrap().len(), 0);

        std::fs::write(&source, b"other").unwrap();
        std::fs::write(&temp.source_id_path, source_id(&source).unwrap()).unwrap();
        std::fs::write(&source, b"third").unwrap();
        assert_eq!(crate::scanner::scan(&input, &output).unwrap().len(), 1, "a marker outlived its file");
    }

    #[test]
    fn a_source_that_turned_unreadable_is_the_same_file_but_not_archived() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let (source, temp, chunk) = claimed_with_chunk(dir.path(), b"video");
        temp.mark_delivered(false).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&source).is_ok() {
            return; // running as root, where nothing is unreadable
        }

        let hashed = temp.record();
        assert!(temp.source_unchanged(&source));
        assert!(!temp.awaits_archiving(&source));
        temp.claim_source(&source, "Film").unwrap();
        assert!(chunk.exists());
        assert_eq!(temp.record(), hashed, "a failed read cost the record its hash");
    }

    #[test]
    fn a_source_unreadable_from_the_start_is_recorded_and_a_new_file_lifts_its_marker() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let (input, output) = (dir.path().join("input"), dir.path().join("output"));
        let profile = input.join("p");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        let source = profile.join("film.mkv");
        std::fs::write(&source, b"video").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&source).is_ok() {
            return; // running as root, where nothing is unreadable
        }

        let temp = TempDir::for_video(&output, "film");
        temp.claim_source(&source, "film").unwrap();
        assert_eq!(temp.record(), Some(format!("{}\n5", source.display())));
        std::fs::write(&temp.failed_path, b"Permission denied").unwrap();
        assert_eq!(crate::scanner::scan(&input, &output).unwrap().len(), 0);

        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, b"another video").unwrap();
        assert_eq!(crate::scanner::scan(&input, &output).unwrap().len(), 1, "the new file stayed locked out");
    }

    #[test]
    fn a_second_write_replaces_a_symlink_at_the_scratch_name_and_spares_its_target() {
        let dir = tempfile::TempDir::new().unwrap();
        let precious = dir.path().join("precious.txt");
        std::fs::write(&precious, b"only copy").unwrap();
        let state = dir.path().join("source.path");
        std::os::unix::fs::symlink(&precious, dir.path().join("source.path.tmp")).unwrap();

        write_atomic(&state, b"new").unwrap();
        assert_eq!(std::fs::read(&precious).unwrap(), b"only copy");
        assert_eq!(std::fs::read(&state).unwrap(), b"new");
        assert!(!dir.path().join("source.path.tmp").is_symlink());
    }

    #[test]
    fn claiming_a_temp_dir_removes_the_links_in_it_and_spares_their_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::TempDir::new().unwrap();
        let (source, temp, chunk) = claimed_with_chunk(dir.path(), b"video");
        let precious = dir.path().join("precious.txt");
        std::fs::write(&precious, b"only copy").unwrap();
        let recorded = temp.record();

        symlink(&precious, &temp.mux_path).unwrap();
        symlink(&precious, temp.chunk_path("00002")).unwrap();
        symlink(dir.path().join("nothing"), &temp.video_path).unwrap();
        temp.claim_source(&source, "Film").unwrap();

        assert!(!temp.mux_path.is_symlink() && !temp.video_path.is_symlink());
        assert!(!temp.chunk_path("00002").is_symlink());
        assert_eq!(std::fs::read(&precious).unwrap(), b"only copy");
        assert!(chunk.exists(), "a real chunk went with the links");
        assert_eq!(temp.record(), recorded);
    }

    #[test]
    fn a_link_at_chunks_or_at_the_source_record_is_replaced_by_the_real_thing() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("Film.mkv");
        std::fs::write(&source, b"video").unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep.ivf"), b"kept").unwrap();
        let record = dir.path().join("record");
        std::fs::write(&record, source_id(&source).unwrap()).unwrap();

        let temp = TempDir::for_video(dir.path(), "Film");
        std::fs::create_dir(&temp.path).unwrap();
        symlink(&elsewhere, &temp.chunks_dir).unwrap();
        symlink(&record, &temp.source_id_path).unwrap();
        temp.claim_source(&source, "Film").unwrap();

        assert!(temp.chunks_dir.is_dir() && !temp.chunks_dir.is_symlink());
        assert!(!temp.source_id_path.is_symlink());
        assert_eq!(temp.record(), Some(source_id(&source).unwrap()));
        assert_eq!(std::fs::read(elsewhere.join("keep.ivf")).unwrap(), b"kept");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 1);

        let chunk = temp.chunk_path("00001");
        std::fs::write(&chunk, b"chunk").unwrap();
        std::fs::write(&record, "/input/Other.mkv\n7").unwrap();
        std::fs::remove_file(&temp.source_id_path).unwrap();
        symlink(&record, &temp.source_id_path).unwrap();
        temp.claim_source(&source, "Film").unwrap();

        assert!(!chunk.exists(), "a link naming another video kept its work");
        assert_eq!(temp.record(), Some(source_id(&source).unwrap()));
        assert_eq!(std::fs::read_to_string(&record).unwrap(), "/input/Other.mkv\n7");
    }

    #[test]
    fn a_temp_dir_that_is_a_symlink_is_replaced_and_its_target_spared() {
        let dir = tempfile::TempDir::new().unwrap();
        let finished = dir.path().join("Finished");
        std::fs::create_dir_all(finished.join("Season 1")).unwrap();
        std::fs::write(finished.join("movie.mkv"), b"finished").unwrap();
        std::fs::write(finished.join("Season 1/episode.mkv"), b"finished").unwrap();
        let source = dir.path().join("Film.mkv");
        std::fs::write(&source, b"video").unwrap();
        std::os::unix::fs::symlink(&finished, dir.path().join(".avet_Film")).unwrap();

        let temp = TempDir::for_video(dir.path(), "Film");
        assert!(!temp.path.is_symlink());
        temp.claim_source(&source, "Film").unwrap();
        assert!(temp.path.is_dir() && !temp.path.is_symlink());
        std::fs::write(temp.chunk_path("00001"), b"chunk").unwrap();
        temp.mark_delivered(false).unwrap();
        temp.clear_work().unwrap();
        temp.finish_delivery().unwrap();

        assert_eq!(std::fs::read(finished.join("movie.mkv")).unwrap(), b"finished");
        assert_eq!(std::fs::read(finished.join("Season 1/episode.mkv")).unwrap(), b"finished");
        assert_eq!(std::fs::read_dir(&finished).unwrap().count(), 2);
    }

    #[test]
    fn a_symlink_that_cannot_be_removed_is_never_written_or_cleared_through() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let finished = dir.path().join("Finished");
        std::fs::create_dir_all(&finished).unwrap();
        std::fs::write(finished.join("movie.mkv"), b"finished").unwrap();
        std::fs::write(finished.join("delivered"), b"keep").unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir_all(&output).unwrap();
        let source = dir.path().join("Film.mkv");
        std::fs::write(&source, b"video").unwrap();
        std::fs::write(finished.join("source.path"), source_id(&source).unwrap()).unwrap();
        std::os::unix::fs::symlink(&finished, output.join(".avet_Film")).unwrap();
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o555)).unwrap();

        let temp = TempDir::for_video(&output, "Film");
        let held = temp.path.is_symlink();
        let refused = held
            && temp.create_dirs().is_err()
            && temp.claim_source(&source, "Film").is_err()
            && temp.clear_work().is_err()
            && temp.finish_delivery().is_err()
            && !temp.awaits_archiving(&source);
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !held {
            return; // running as root, where the folder's mode stops nobody
        }

        assert!(refused);
        assert!(!finished.join("chunks").exists());
        assert_eq!(std::fs::read(finished.join("movie.mkv")).unwrap(), b"finished");
        assert_eq!(std::fs::read(finished.join("delivered")).unwrap(), b"keep");
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

        assert!(reloaded.unmeasured("00001"));
        assert!(!reloaded.unmeasured("00002") && !reloaded.unmeasured("00003"));
        reloaded.set_measured("00001", 9.47).unwrap();
        reloaded.set_measured("00002", 9.5).unwrap();
        let again = CrfCache::load_or_create(&path).unwrap();
        assert!(!again.unmeasured("00001"));
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
