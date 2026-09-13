use std::path::{Path, PathBuf};

use crate::ffms2::{PixelSubsampling, VideoInfo};

pub fn calculate(info: &VideoInfo, stem: &str, threads_per_worker: usize) -> usize {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let ram_gb = available_ram_gib();

    let megapixels = (info.width as f64 * info.height as f64) / 1_000_000.0;

    let pix_mult = match info.pixel_format.subsampling {
        PixelSubsampling::Yuv444 => 1.5,
        PixelSubsampling::Yuv422 => 1.25,
        PixelSubsampling::Yuv420 => 1.0,
    };

    const CM_RAM: f64 = 0.3;
    const ENC_RAM: f64 = 1.2;

    let by_cpu = cpu_cores / threads_per_worker;
    let ram_per_worker = megapixels * (ENC_RAM + CM_RAM) * pix_mult;
    // floor, not round: the cgroup answers an extra worker with an OOM kill mid-chunk.
    let by_ram = if ram_per_worker > 0.0 {
        (ram_gb / ram_per_worker).floor() as usize
    } else {
        usize::MAX
    };

    let workers = by_cpu.min(by_ram).max(1);

    tracing::info!(
        "[{stem}] workers: {workers} \
         (cpu={cpu_cores}/{threads_per_worker} threads allows {by_cpu}, \
         ram={ram_gb:.0}GB/{ram_per_worker:.1}GB allows {by_ram})"
    );

    workers
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
    std::fs::read_to_string("/proc/self/cgroup")
        .unwrap_or_default()
        .lines()
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

    fn info(w: u32, h: u32, sub: PixelSubsampling) -> VideoInfo {
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
                subsampling: sub,
            },
        }
    }

    #[test]
    fn workers_at_least_one() {
        let i = info(1920, 1080, PixelSubsampling::Yuv420);
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
        let sample = "0::/system.slice/avet.service\n\
                      4:memory:/docker/abc123\n";
        let map: std::collections::HashMap<String, String> = sample
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, ':');
                parts.next()?;
                Some((parts.next()?.to_string(), parts.next()?.to_string()))
            })
            .collect();
        assert_eq!(map.get("").map(String::as_str), Some("/system.slice/avet.service"));
        assert_eq!(map.get("memory").map(String::as_str), Some("/docker/abc123"));
    }

    #[test]
    fn workers_4k_hdr_fewer_than_1080p() {
        let hd = info(1920, 1080, PixelSubsampling::Yuv420);
        let uhd = info(3840, 2160, PixelSubsampling::Yuv444);
        let w_hd = calculate(&hd, "test", 6);
        let w_uhd = calculate(&uhd, "test", 6);
        assert!(w_hd >= w_uhd, "4K/444 should use <= workers than 1080p/420");
    }
}
