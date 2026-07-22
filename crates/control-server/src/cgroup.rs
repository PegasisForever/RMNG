//! CT 105 clone cgroup-v2 memory counters read through the shared PID namespace.
//!
//! The control-server runs with `pid: "host"`, so `/proc/<clone-pid>/root` exposes a clone's
//! own cgroup namespace. Reading counters there avoids Docker's presentation-oriented memory
//! figure: we can include swap and keep tmpfs/shmem while excluding reclaimable page cache.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::statvfs::statvfs;

const LXC_CGROUP_ROOT: &str = "/proc/1/root/sys/fs/cgroup";
const LXC_ROOT: &str = "/proc/1/root";

/// RAM plus swap usage and limit for one clone, in bytes. A zero limit means one or both
/// cgroup limits are unbounded or unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryUsage {
    pub used: u64,
    pub limit: u64,
}

/// Read cgroup-v2 memory counters through a CT 105 clone's `/proc/<pid>/root` filesystem view.
/// Callers retain their prior sample when this returns an error: the container may have stopped,
/// the control-server may lack `pid: "host"`, or a counter may be incomplete or malformed.
pub async fn memory_usage(pid: i64) -> Result<MemoryUsage> {
    memory_usage_from_root(&PathBuf::from(format!("/proc/{pid}/root/sys/fs/cgroup"))).await
}

/// CT 105's whole-LXC memory usage through the control-server's shared PID namespace.
pub async fn lxc_memory_usage() -> Result<MemoryUsage> {
    memory_usage_from_root(Path::new(LXC_CGROUP_ROOT)).await
}

/// CT 105's cumulative CPU time, in microseconds, including every descendant cgroup.
pub async fn lxc_cpu_usage_usec() -> Result<u64> {
    let path = Path::new(LXC_CGROUP_ROOT).join("cpu.stat");
    let stat = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    parse_cpu_usage(&stat)
}

/// Physical, compression-aware use of CT 105's ZFS root filesystem, in bytes.
pub fn lxc_disk_used() -> Result<u64> {
    let stat = statvfs(LXC_ROOT).with_context(|| format!("reading filesystem stats for {LXC_ROOT}"))?;
    disk_used(stat.blocks(), stat.blocks_free(), stat.fragment_size())
}

/// Kept separate from the `/proc` path so synthetic CT 105 cgroup-v2 fixtures can exercise the
/// reader without a Docker daemon.
async fn memory_usage_from_root(root: &Path) -> Result<MemoryUsage> {
    let (current, swap, memory_max, swap_max, stat) = tokio::join!(
        read_counter(root.join("memory.current")),
        read_counter(root.join("memory.swap.current")),
        read_limit(root.join("memory.max")),
        read_limit(root.join("memory.swap.max")),
        tokio::fs::read_to_string(root.join("memory.stat")),
    );
    let (inactive_file, shmem) = parse_memory_stat(&stat.context("reading cgroup memory.stat")?)?;
    let used = current?
        .saturating_sub(inactive_file.saturating_sub(shmem))
        .saturating_add(swap?);
    let limit = memory_max?
        .zip(swap_max?)
        .and_then(|(memory, swap)| memory.checked_add(swap))
        .unwrap_or(0);

    Ok(MemoryUsage { used, limit })
}

async fn read_counter(path: PathBuf) -> Result<u64> {
    let text = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    text.trim()
        .parse()
        .with_context(|| format!("parsing counter {}", path.display()))
}

/// `None` represents cgroup-v2's `max` sentinel.
async fn read_limit(path: PathBuf) -> Result<Option<u64>> {
    let value = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    match value.trim() {
        "max" => Ok(None),
        value => value
            .parse()
            .map(Some)
            .with_context(|| format!("parsing limit {}", path.display())),
    }
}

/// Extract the two counters that distinguish reclaimable page cache from shared memory. Every
/// line must retain cgroup-v2's two-field numeric shape; otherwise this is not a trusted sample.
fn parse_memory_stat(input: &str) -> Result<(u64, u64)> {
    let mut inactive_file = None;
    let mut shmem = None;
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let key = parts
            .next()
            .ok_or_else(|| anyhow!("memory.stat line has no key: {line:?}"))?;
        let value: u64 = parts
            .next()
            .ok_or_else(|| anyhow!("memory.stat line has no value: {line:?}"))?
            .parse()
            .with_context(|| format!("parsing memory.stat line {line:?}"))?;
        if parts.next().is_some() {
            bail!("memory.stat line has extra fields: {line:?}");
        }
        match key {
            "inactive_file" => inactive_file = Some(value),
            "shmem" => shmem = Some(value),
            _ => {}
        }
    }
    Ok((
        inactive_file.ok_or_else(|| anyhow!("memory.stat is missing inactive_file"))?,
        shmem.ok_or_else(|| anyhow!("memory.stat is missing shmem"))?,
    ))
}

fn parse_cpu_usage(input: &str) -> Result<u64> {
    let mut usage = None;
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let key = parts
            .next()
            .ok_or_else(|| anyhow!("cpu.stat line has no key: {line:?}"))?;
        let value: u64 = parts
            .next()
            .ok_or_else(|| anyhow!("cpu.stat line has no value: {line:?}"))?
            .parse()
            .with_context(|| format!("parsing cpu.stat line {line:?}"))?;
        if parts.next().is_some() {
            bail!("cpu.stat line has extra fields: {line:?}");
        }
        if key == "usage_usec" {
            usage = Some(value);
        }
    }
    usage.ok_or_else(|| anyhow!("cpu.stat is missing usage_usec"))
}

fn disk_used(blocks: u64, blocks_free: u64, fragment_size: u64) -> Result<u64> {
    blocks
        .checked_sub(blocks_free)
        .and_then(|used_blocks| used_blocks.checked_mul(fragment_size))
        .ok_or_else(|| anyhow!("filesystem usage overflow or invalid free-block count"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

    fn test_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rmng-cgroup-test-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, path: &str, value: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value).unwrap();
    }

    #[test]
    fn parses_cache_and_shmem_strictly() {
        assert_eq!(
            parse_memory_stat("anon 12\ninactive_file 30\nshmem 8\n").unwrap(),
            (30, 8)
        );
        assert!(parse_memory_stat("inactive_file nope\nshmem 0\n").is_err());
        assert!(parse_memory_stat("inactive_file 1\n").is_err());
    }

    #[test]
    fn parses_ct_cpu_usage_and_physical_disk_blocks() {
        assert_eq!(parse_cpu_usage("usage_usec 120\nuser_usec 80\n").unwrap(), 120);
        assert!(parse_cpu_usage("user_usec 80\n").is_err());
        assert!(parse_cpu_usage("usage_usec nope\n").is_err());
        assert!(parse_cpu_usage("usage_usec 1 extra\n").is_err());

        assert_eq!(disk_used(10, 3, 4096).unwrap(), 28_672);
        assert!(disk_used(3, 10, 4096).is_err());
        assert!(disk_used(u64::MAX, 0, 2).is_err());
    }

    #[tokio::test]
    async fn reads_ct105_v2_counters() {
        let root = test_root();
        write(&root, "memory.current", "100\n");
        write(&root, "memory.swap.current", "30\n");
        write(&root, "memory.max", "128\n");
        write(&root, "memory.swap.max", "64\n");
        write(&root, "memory.stat", "inactive_file 60\nshmem 20\n");

        assert_eq!(
            memory_usage_from_root(&root).await.unwrap(),
            MemoryUsage { used: 90, limit: 192 }
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unbounded_or_overflowed_limit_is_zero() {
        let root = test_root();
        write(&root, "memory.current", "10\n");
        write(&root, "memory.swap.current", "0\n");
        write(&root, "memory.max", "max\n");
        write(&root, "memory.swap.max", "64\n");
        write(&root, "memory.stat", "inactive_file 0\nshmem 0\n");
        assert_eq!(memory_usage_from_root(&root).await.unwrap().limit, 0);
        write(&root, "memory.max", &format!("{}\n", u64::MAX));
        write(&root, "memory.swap.max", "1\n");
        assert_eq!(memory_usage_from_root(&root).await.unwrap().limit, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn malformed_or_missing_counters_are_not_samples() {
        let root = test_root();
        write(&root, "memory.current", "100\n");
        write(&root, "memory.swap.current", "0\n");
        write(&root, "memory.max", "128\n");
        write(&root, "memory.swap.max", "64\n");
        write(&root, "memory.stat", "inactive_file malformed\nshmem 0\n");

        assert!(memory_usage_from_root(&root).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
