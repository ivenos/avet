use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::resume::TempDir;

#[derive(Debug)]
pub struct Job {
    pub encode_toml: PathBuf,
    pub source_file: PathBuf,
    pub rel_dir: PathBuf,
}

impl Job {
    /// Always UTF-8: `find_video_files` filters non-UTF8 names before Jobs are constructed.
    pub fn stem(&self) -> &str {
        self.source_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("video")
    }

    pub fn output_dir(&self, output_root: &Path) -> PathBuf {
        output_root.join(&self.rel_dir)
    }
}

pub fn scan(input_dir: &Path, output_dir: &Path) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();

    let mut profile_dirs: Vec<PathBuf> = std::fs::read_dir(input_dir)
        .with_context(|| format!("read {}", input_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    profile_dirs.sort();

    for profile_dir in profile_dirs {
        if !profile_dir.is_dir() || profile_dir.file_name() == Some(OsStr::new("processed")) {
            continue;
        }
        if profile_dir.file_name().and_then(OsStr::to_str).is_none() {
            tracing::warn!("skipping profile folder with non-UTF8 name: {}", profile_dir.display());
            continue;
        }

        let encode_toml = profile_dir.join("encode.toml");
        if !encode_toml.exists() {
            continue;
        }

        for (source_file, rel_dir) in find_video_files(&profile_dir) {
            let job = Job { encode_toml: encode_toml.clone(), source_file, rel_dir };
            if output_exists(&job.output_dir(output_dir), &job.source_file) {
                tracing::debug!(file = %job.source_file.display(), "skip: output exists");
                continue;
            }
            jobs.push(job);
        }
    }

    // Before the marker filter: a marker hiding one side of a name clash lets the other
    // side run, and its `claim_source` wipes the temp dir the marker lives in.
    let jobs = drop_name_collisions(jobs)
        .into_iter()
        .filter(|job| match failed_marker(&job.output_dir(output_dir), &job.source_file) {
            Some(marker) => {
                report(format!("[{}] permanently failed - delete {} to retry", job.stem(), marker.display()));
                false
            }
            None => true,
        })
        .collect();

    Ok(jobs)
}

/// Folder and stem name output, temp dir and archive, so two files sharing both stop.
fn drop_name_collisions(jobs: Vec<Job>) -> Vec<Job> {
    let name = |j: &Job| j.rel_dir.join(j.stem());
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();
    for job in &jobs {
        *seen.entry(name(job)).or_insert(0) += 1;
    }

    let colliding: Vec<PathBuf> = seen
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(name, _)| name)
        .collect();

    for c in &colliding {
        let paths: Vec<String> = jobs
            .iter()
            .filter(|j| name(j) == *c)
            .map(|j| j.source_file.display().to_string())
            .collect();
        report(format!(
            "[{}] skipping {} files that share this name - they would overwrite each \
             other's output: {}",
            c.display(),
            paths.len(),
            paths.join(", ")
        ));
    }

    jobs.into_iter()
        .filter(|j| !colliding.contains(&name(j)))
        .collect()
}

fn report(message: String) {
    static SEEN: std::sync::Mutex<BTreeSet<String>> = std::sync::Mutex::new(BTreeSet::new());
    if SEEN.lock().map_or(true, |mut seen| seen.insert(message.clone())) {
        tracing::warn!("{message}");
    } else {
        tracing::debug!("{message}");
    }
}

fn find_video_files(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut files = Vec::new();
    collect_video_files(dir, Path::new(""), &mut files);
    files.sort_by(|a, b| (&a.1, a.0.file_name()).cmp(&(&b.1, b.0.file_name())));
    files
}

fn collect_video_files(dir: &Path, rel: &Path, files: &mut Vec<(PathBuf, PathBuf)>) {
    const EXTENSIONS: &[&str] = &["mkv", "mp4", "mov", "avi", "ts", "m2ts", "flv", "webm", "m4v"];

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("skipping {}: {e}", dir.display());
            return;
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            tracing::warn!("skipping an unreadable entry in {}", dir.display());
            continue;
        };
        let path = entry.path();
        // DirEntry's type does not follow symlinks, so a link back up cannot loop.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            match path.file_name().and_then(|n| n.to_str()) {
                None => tracing::warn!("skipping folder with non-UTF8 name: {}", path.display()),
                Some(name) if name.starts_with('.') => {}
                Some(name) => collect_video_files(&path, &rel.join(name), files),
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        // macOS writes ._Name.mkv beside Name.mkv; it is metadata, not a video.
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.')) {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase());
        if let Some(ext) = ext
            && EXTENSIONS.contains(&ext.as_str())
        {
            // Skip non-UTF8 stems: they'd collide on the fallback name and break temp-dir layout.
            if path.file_stem().and_then(|s| s.to_str()).is_none() {
                tracing::warn!("skipping file with non-UTF8 name: {}", path.display());
                continue;
            }
            files.push((path, rel.to_path_buf()));
        }
    }
}

/// avet never produces an empty output, so one is a leftover, not "already done".
fn output_exists(output_dir: &Path, source_file: &Path) -> bool {
    let stem = source_file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let path = output_dir.join(format!("{stem}.mkv"));
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > 0 => true,
        Ok(_) => {
            tracing::warn!("[{stem}] ignoring empty output file {}", path.display());
            false
        }
        Err(_) => false,
    }
}

/// The marker path when this exact source is locked out; a stem twin does not block.
fn failed_marker(output_dir: &Path, source_file: &Path) -> Option<PathBuf> {
    let stem = source_file.file_stem().and_then(|s| s.to_str())?;
    let temp = TempDir::for_video(output_dir, stem);
    if !temp.failed_path.exists() {
        return None;
    }
    match temp.recorded_id() {
        Some(prev) if prev != crate::resume::source_id(source_file) => None,
        _ => Some(temp.failed_path),
    }
}

pub fn ensure_processed_dir(input_dir: &Path, rel_dir: &Path) -> Result<PathBuf> {
    let processed = input_dir.join("processed").join(rel_dir);
    std::fs::create_dir_all(&processed)
        .with_context(|| format!("create {}", processed.display()))?;
    Ok(processed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_dirs() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let input = tmp.path().join("input");
        let output = tmp.path().join("output");
        fs::create_dir_all(&input).unwrap();
        fs::create_dir_all(&output).unwrap();
        (tmp, input, output)
    }

    #[test]
    fn scan_finds_profile_with_video() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("test-profile");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();

        let jobs = scan(&input, &output).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].source_file.file_name().unwrap(), "film.mkv");
    }

    #[test]
    fn names_with_a_line_break_are_picked_up() {
        let (_tmp, input, output) = make_dirs();
        let folder = input.join("p").join("Show\nPart 2");
        fs::create_dir_all(&folder).unwrap();
        fs::write(input.join("p").join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(folder.join("film\r\n.mkv"), b"fake").unwrap();

        let jobs = scan(&input, &output).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].stem(), "film\r\n");
        assert_eq!(jobs[0].rel_dir, PathBuf::from("Show\nPart 2"));
    }

    #[test]
    fn scan_skips_processed_dir() {
        let (_tmp, input, output) = make_dirs();
        let processed = input.join("processed");
        fs::create_dir_all(&processed).unwrap();
        fs::write(processed.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(processed.join("film.mkv"), b"fake").unwrap();

        let jobs = scan(&input, &output).unwrap();
        assert_eq!(jobs.len(), 0);
    }

    #[test]
    fn scan_skips_existing_output() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("p");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();
        fs::write(output.join("film.mkv"), b"done").unwrap();

        let jobs = scan(&input, &output).unwrap();
        assert_eq!(jobs.len(), 0);
    }

    #[test]
    fn an_empty_output_file_is_a_leftover_not_a_finished_encode() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("p");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();
        fs::write(output.join("film.mkv"), b"").unwrap();

        assert_eq!(scan(&input, &output).unwrap().len(), 1);
    }

    #[test]
    fn scan_skips_dir_without_toml() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("no-toml");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();

        let jobs = scan(&input, &output).unwrap();
        assert_eq!(jobs.len(), 0);
    }

    #[test]
    fn scan_drops_files_that_share_a_stem() {
        let (_tmp, input, output) = make_dirs();
        for p in ["a", "b"] {
            let profile = input.join(p);
            fs::create_dir_all(&profile).unwrap();
            fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
            fs::write(profile.join("film.mkv"), b"fake").unwrap();
        }
        assert_eq!(scan(&input, &output).unwrap().len(), 0);
    }

    #[test]
    fn scan_drops_same_stem_with_different_extensions() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("p");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();
        fs::write(profile.join("film.mp4"), b"fake").unwrap();
        assert_eq!(scan(&input, &output).unwrap().len(), 0);
    }

    #[test]
    fn scan_keeps_distinct_stems_next_to_a_collision() {
        let (_tmp, input, output) = make_dirs();
        for p in ["a", "b"] {
            let profile = input.join(p);
            fs::create_dir_all(&profile).unwrap();
            fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
            fs::write(profile.join("film.mkv"), b"fake").unwrap();
        }
        fs::write(input.join("a").join("other.mkv"), b"fake").unwrap();
        let jobs = scan(&input, &output).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].stem(), "other");
    }

    #[test]
    fn a_folder_inside_a_profile_is_kept_as_the_job_folder() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("p");
        let s1 = profile.join("Show").join("Season 1");
        let s2 = profile.join("Show").join("Season 2");
        fs::create_dir_all(&s1).unwrap();
        fs::create_dir_all(&s2).unwrap();
        fs::create_dir_all(profile.join(".hidden")).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();
        fs::write(s1.join("Episode 01.mkv"), b"fake").unwrap();
        fs::write(s2.join("Episode 01.mkv"), b"fake").unwrap();
        fs::write(profile.join(".hidden").join("skip.mkv"), b"fake").unwrap();

        let jobs = scan(&input, &output).unwrap();
        let rel: Vec<_> = jobs.iter().map(|j| j.rel_dir.join(j.stem())).collect();
        assert_eq!(rel, vec![
            PathBuf::from("film"),
            PathBuf::from("Show/Season 1/Episode 01"),
            PathBuf::from("Show/Season 2/Episode 01"),
        ]);

        fs::create_dir_all(output.join("Show").join("Season 1")).unwrap();
        fs::write(output.join("Show").join("Season 1").join("Episode 01.mkv"), b"done").unwrap();
        assert_eq!(scan(&input, &output).unwrap().len(), 2);
    }

    #[test]
    fn a_folder_symlinked_back_up_is_not_followed() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("p");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();
        std::os::unix::fs::symlink(&profile, profile.join("loop")).unwrap();
        assert_eq!(scan(&input, &output).unwrap().len(), 1);
    }

    #[test]
    fn a_macos_resource_fork_is_not_a_job() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("p");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();
        fs::write(profile.join("._film.mkv"), b"apple double").unwrap();

        let jobs = scan(&input, &output).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].stem(), "film");
    }

    #[test]
    fn a_profile_folder_with_a_non_utf8_name_is_skipped_not_failed() {
        use std::os::unix::ffi::OsStrExt;

        let (_tmp, input, output) = make_dirs();
        let profile = input.join(OsStr::from_bytes(b"Filme_\xe4"));
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();

        assert_eq!(scan(&input, &output).unwrap().len(), 0);
    }

    #[test]
    fn an_unreadable_folder_does_not_stop_the_scan() {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, input, output) = make_dirs();
        for name in ["a", "b"] {
            let profile = input.join(name);
            fs::create_dir_all(&profile).unwrap();
            fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
            fs::write(profile.join(format!("{name}.mkv")), b"fake").unwrap();
        }

        let locked = input.join("a").join("season");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            return; // running as root, where nothing is unreadable
        }

        let jobs = scan(&input, &output).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(jobs.len(), 2);
    }

    #[test]
    fn a_marker_on_one_half_of_a_name_clash_still_stops_the_other() {
        let (_tmp, input, output) = make_dirs();
        for p in ["a", "b"] {
            let profile = input.join(p);
            fs::create_dir_all(&profile).unwrap();
            fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
            fs::write(profile.join("film.mkv"), b"fake").unwrap();
        }

        let temp = crate::resume::TempDir::for_video(&output, "film");
        temp.create_dirs().unwrap();
        fs::write(&temp.failed_path, b"boom").unwrap();
        fs::write(&temp.source_id_path, crate::resume::source_id(&input.join("a").join("film.mkv"))).unwrap();

        // Running b would discard the temp dir the marker of a lives in.
        assert_eq!(scan(&input, &output).unwrap().len(), 0);
    }

    #[test]
    fn failed_marker_only_blocks_the_file_it_was_written_for() {
        let (_tmp, input, output) = make_dirs();
        let profile = input.join("p");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("encode.toml"), b"encoder = \"svt-av1\"\n").unwrap();
        fs::write(profile.join("film.mkv"), b"fake").unwrap();

        let temp = crate::resume::TempDir::for_video(&output, "film");
        temp.create_dirs().unwrap();
        fs::write(&temp.failed_path, b"boom").unwrap();

        // Marker written for this exact file: blocked.
        fs::write(&temp.source_id_path, crate::resume::source_id(&profile.join("film.mkv"))).unwrap();
        assert_eq!(scan(&input, &output).unwrap().len(), 0);

        // Marker left over from a different file that had the same name: not blocked.
        fs::write(&temp.source_id_path, "/somewhere/else/film.mkv").unwrap();
        assert_eq!(scan(&input, &output).unwrap().len(), 1);
    }
}
