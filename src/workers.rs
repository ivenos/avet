use std::path::{Path, PathBuf};

use crate::ffms2::VideoInfo;

pub fn calculate(info: &VideoInfo, stem: &str, threads_per_worker: usize) -> usize {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let ram_gb = available_ram_gib();
    let megapixels = (info.width as f64 * info.height as f64) / 1_000_000.0;

    let Split { workers, by_cpu, by_ram, ram_per_worker } = split(cpu_cores, ram_gb, megapixels, threads_per_worker);

    tracing::info!(
        "[{stem}] workers: {workers} \
         (cpu={cpu_cores}/{threads_per_worker} threads allows {by_cpu}, \
         ram={ram_gb:.0}GB/{ram_per_worker:.1}GB allows {by_ram})"
    );

    workers
}

struct Split {
    workers: usize,
    by_cpu: usize,
    by_ram: usize,
    ram_per_worker: f64,
}

fn split(cpu_cores: usize, ram_gb: f64, megapixels: f64, threads_per_worker: usize) -> Split {
    const CM_RAM: f64 = 0.3;
    const ENC_RAM: f64 = 1.2;

    let by_cpu = cpu_cores / threads_per_worker;
    let ram_per_worker = megapixels * (ENC_RAM + CM_RAM);
    let by_ram = if ram_per_worker > 0.0 {
        (ram_gb / ram_per_worker).floor() as usize
    } else {
        usize::MAX
    };
    Split { workers: by_cpu.min(by_ram).max(1), by_cpu, by_ram, ram_per_worker }
}

fn available_ram_gib() -> f64 {
    let host = meminfo_available_gib().unwrap_or(1.0);
    match cgroup_available_gib() {
        Some(limit) => host.min(limit),
        None => host,
    }
}

fn meminfo_available_gib() -> Option<f64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u64>().ok())
        .map(|kb| kb as f64 / 1_048_576.0)
}

/// /proc/meminfo reports host RAM inside a container, and the limit can sit on any
/// ancestor rather than the mount root, so every level is read and the tightest wins.
fn cgroup_available_gib() -> Option<f64> {
    let own = own_cgroup_paths();

    let v2 = cgroup_dirs("/sys/fs/cgroup", own.get("").map(String::as_str))
        .filter_map(|dir| headroom(&dir.join("memory.max"), &dir.join("memory.current")));
    let v1 = cgroup_dirs("/sys/fs/cgroup/memory", own.get("memory").map(String::as_str))
        .filter_map(|dir| {
            headroom(
                &dir.join("memory.limit_in_bytes"),
                &dir.join("memory.usage_in_bytes"),
            )
        });

    v2.chain(v1).reduce(f64::min)
}

/// Keyed by controller; v2's unified hierarchy is the empty string (`0::/path`).
fn own_cgroup_paths() -> std::collections::HashMap<String, String> {
    parse_cgroup_paths(&std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default())
}

fn parse_cgroup_paths(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let _id = parts.next()?;
            let controllers = parts.next()?;
            let path = parts.next()?;
            Some((controllers.to_string(), path.to_string()))
        })
        .collect()
}

/// The cgroup directory for `rel` under `root`, then each ancestor up to `root` itself.
fn cgroup_dirs(root: &str, rel: Option<&str>) -> impl Iterator<Item = PathBuf> {
    let root = PathBuf::from(root);
    let mut dirs = vec![root.clone()];

    // The path is absolute inside its hierarchy, so joining it verbatim discards `root`.
    let mut current = root;
    for part in rel.unwrap_or("").split('/').filter(|p| !p.is_empty()) {
        current = current.join(part);
        dirs.push(current.clone());
    }
    dirs.into_iter()
}

fn headroom(limit_path: &Path, usage_path: &Path) -> Option<f64> {
    // Unlimited is "max" on v2 (fails to parse) and a near-u64::MAX sentinel on v1.
    let limit: u64 = std::fs::read_to_string(limit_path).ok()?.trim().parse().ok()?;
    if limit >= u64::MAX / 2 {
        return None;
    }
    let usage: u64 = std::fs::read_to_string(usage_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // The page cache is charged here and reclaimed only under pressure.
    let reclaimable = reclaimable_bytes(limit_path);
    let used = usage.saturating_sub(reclaimable);
    Some(limit.saturating_sub(used) as f64 / 1_073_741_824.0)
}

/// File-backed pages in the same cgroup, which the kernel can drop on demand.
fn reclaimable_bytes(limit_path: &Path) -> u64 {
    let Some(stat) = limit_path.parent().map(|d| d.join("memory.stat")) else {
        return 0;
    };
    let Ok(text) = std::fs::read_to_string(stat) else {
        return 0;
    };
    // v2 spells it `inactive_file`, v1 `total_inactive_file`.
    text.lines()
        .find_map(|l| {
            let (key, value) = l.split_once(' ')?;
            (key == "inactive_file" || key == "total_inactive_file")
                .then(|| value.trim().parse::<u64>().ok())?
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffms2::{PixelFormat, PixelSubsampling};

    fn info(w: u32, h: u32) -> VideoInfo {
        VideoInfo {
            width: w,
            height: h,
            fps_num: 24,
            fps_den: 1,
            sar_num: 1,
            sar_den: 1,
            num_frames: 100,
            pixel_format: PixelFormat {
                pix_fmt: 0,
                bit_depth: 10,
                subsampling: PixelSubsampling::Yuv420,
            },
        }
    }

    #[test]
    fn workers_split_by_cores_and_by_ram_whichever_allows_fewer() {
        const HD: f64 = 1920.0 * 1080.0 / 1e6;
        const UHD: f64 = 3840.0 * 2160.0 / 1e6;

        assert_eq!(split(24, 64.0, HD, 6).workers, 4);
        assert_eq!(split(24, 64.0, UHD, 6).workers, 4);
        assert_eq!(split(24, 20.0, UHD, 6).workers, 1);
        assert_eq!(split(24, 20.0, HD, 6).workers, 4);
        assert_eq!(split(2, 64.0, HD, 6).workers, 1);
        assert_eq!(split(24, 0.5, HD, 6).workers, 1);
    }

    #[test]
    fn workers_at_least_one() {
        let i = info(1920, 1080);
        assert!(calculate(&i, "test", 6) >= 1);
    }

    #[test]
    fn cgroup_lookup_walks_from_the_root_down_to_the_process_own_group() {
        // Docker's default namespace maps the process to "/".
        let dirs: Vec<_> = cgroup_dirs("/sys/fs/cgroup", Some("/")).collect();
        assert_eq!(dirs, vec![PathBuf::from("/sys/fs/cgroup")]);

        // A systemd unit or a k8s pod carries the limit on an ancestor.
        let dirs: Vec<_> = cgroup_dirs("/sys/fs/cgroup", Some("/system.slice/avet.service"))
            .collect();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/sys/fs/cgroup"),
                PathBuf::from("/sys/fs/cgroup/system.slice"),
                PathBuf::from("/sys/fs/cgroup/system.slice/avet.service"),
            ]
        );

        let dirs: Vec<_> = cgroup_dirs("/sys/fs/cgroup", None).collect();
        assert_eq!(dirs, vec![PathBuf::from("/sys/fs/cgroup")]);
    }

    #[test]
    fn own_cgroup_paths_reads_both_hierarchies() {
        let map = parse_cgroup_paths("0::/system.slice/avet.service\n4:memory:/docker/abc123\n");
        assert_eq!(map.get("").map(String::as_str), Some("/system.slice/avet.service"));
        assert_eq!(map.get("memory").map(String::as_str), Some("/docker/abc123"));
    }
}
