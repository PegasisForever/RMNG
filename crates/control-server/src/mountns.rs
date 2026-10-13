//! Detach copies of one clone home's mounts that live in OTHER mount namespaces.
//!
//! `zfs destroy` answers `dataset is busy` while any mount namespace still holds the
//! dataset, or an overlay whose upper sits on it. The server unmounts its own copies, but
//! the homes bind is `rshared`, so every namespace created while a home was mounted got
//! its own copy: systemd services with `PrivateMounts`/`ProtectSystem` (logind, networkd,
//! resolved, postfix), containerd shims, the server container, and every clone that
//! binds the browse root at `/clones`. Those copies outlive the clone, and the destroy
//! then fails for good.
//!
//! The fix is a raw `umount2(MNT_DETACH)` inside each foreign namespace. util-linux
//! `umount -l` run through `nsenter` fails with `ENOENT` on these mounts; the raw syscall
//! in the same namespace works. Needs `--pid=host` (to see the CT's processes) and a
//! privileged container (for `setns`), which is how the server runs.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// One mount from `/proc/<pid>/mountinfo`: where it is, and what it is.
#[derive(Debug, PartialEq)]
struct MountLine {
    mount_point: String,
    fstype: String,
    source: String,
    super_opts: String,
}

/// Parse one `mountinfo` line. Fields 1–6 are fixed, then optional fields up to a lone
/// `-`, then fstype, source and super options. The mount point is octal-escaped.
fn parse_mountinfo_line(line: &str) -> Option<MountLine> {
    let (head, tail) = line.split_once(" - ")?;
    let mount_point = unescape(head.split(' ').nth(4)?);
    let mut t = tail.split(' ');
    Some(MountLine {
        mount_point,
        fstype: t.next()?.to_string(),
        source: unescape(t.next()?),
        super_opts: t.next().unwrap_or("").to_string(),
    })
}

/// Undo the kernel's `\ooo` escapes (space, tab, newline, backslash).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a mount pins the home: the dataset itself, or an overlay whose upper (or
/// work) dir is inside the dataset's directory.
fn pins_home(m: &MountLine, dataset: &str, dataset_dir: &Path) -> bool {
    if m.fstype == "zfs" {
        return m.source == dataset;
    }
    if m.fstype == "overlay" {
        let dir = format!("{}/", dataset_dir.display());
        return m
            .super_opts
            .split(',')
            .filter_map(|o| o.strip_prefix("upperdir=").or(o.strip_prefix("workdir=")))
            .any(|p| p.starts_with(&dir));
    }
    false
}

/// The mount points in one `mountinfo` that pin the home, deepest first, so a nested
/// mount comes off before the one it sits on.
fn pinning_mounts(mountinfo: &str, dataset: &str, dataset_dir: &Path) -> Vec<String> {
    let mut points: Vec<String> = mountinfo
        .lines()
        .filter_map(parse_mountinfo_line)
        .filter(|m| pins_home(m, dataset, dataset_dir))
        .map(|m| m.mount_point)
        .collect();
    points.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));
    points.dedup();
    points
}

/// Mount namespace inode of a process, or `None` when it is gone or unreadable.
fn ns_ino(pid: &str) -> Option<u64> {
    std::fs::metadata(format!("/proc/{pid}/ns/mnt"))
        .ok()
        .map(|m| m.ino())
}

/// Detach every copy of the home's mounts in every mount namespace except our own.
/// Returns how many mounts came off. Best-effort: a process that exits mid-walk, or a
/// namespace we cannot enter, is skipped with a log line.
pub(crate) fn detach_foreign_copies(dataset: &str, dataset_dir: &Path) -> usize {
    let own = ns_ino("self");
    let mut seen: HashSet<u64> = HashSet::new();
    let mut detached = 0;
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return 0;
    };
    for e in rd.flatten() {
        let pid = e.file_name().to_string_lossy().into_owned();
        if !pid.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Some(ino) = ns_ino(&pid) else { continue };
        if Some(ino) == own || !seen.insert(ino) {
            continue;
        }
        let Ok(info) = std::fs::read_to_string(format!("/proc/{pid}/mountinfo")) else {
            continue;
        };
        let points = pinning_mounts(&info, dataset, dataset_dir);
        if points.is_empty() {
            continue;
        }
        match detach_in_ns(&pid, &points) {
            Ok(n) => {
                tracing::info!(
                    target: "zfs",
                    "detached {n} copies of {dataset} in the mount namespace of pid {pid}"
                );
                detached += n;
            }
            Err(e) => tracing::warn!(
                target: "zfs",
                "detaching {dataset} in the mount namespace of pid {pid}: {e:#}"
            ),
        }
    }
    detached
}

/// `umount2(MNT_DETACH)` each mount point inside `pid`'s mount namespace. Runs on a
/// throwaway thread: `setns` into a mount namespace needs a thread with its own fs
/// context (`unshare(CLONE_FS)`), and that thread must never run anything else after.
fn detach_in_ns(pid: &str, points: &[String]) -> anyhow::Result<usize> {
    use anyhow::Context;
    use nix::sched::{CloneFlags, setns, unshare};
    let ns = std::fs::File::open(format!("/proc/{pid}/ns/mnt")).context("opening ns/mnt")?;
    let points = points.to_vec();
    let pid = pid.to_string();
    std::thread::spawn(move || -> anyhow::Result<usize> {
        unshare(CloneFlags::CLONE_FS).context("unshare(CLONE_FS)")?;
        setns(&ns, CloneFlags::CLONE_NEWNS).context("setns(CLONE_NEWNS)")?;
        let mut n = 0;
        for p in &points {
            match nix::mount::umount2(p.as_str(), nix::mount::MntFlags::MNT_DETACH) {
                Ok(()) => n += 1,
                // Already gone: a parent detach took it, or the process raced us.
                Err(nix::errno::Errno::EINVAL | nix::errno::Errno::ENOENT) => {}
                Err(e) => {
                    tracing::warn!(target: "zfs", "umount2({p}) in pid {pid}'s namespace: {e}")
                }
            }
        }
        Ok(n)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("detach thread panicked"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "\
22496 22339 0:216 / /srv/rmng-homes rw,relatime shared:1785 master:247 - zfs rpool/rmng-homes-101 rw,xattr,noacl
22497 22496 0:119 / /srv/rmng-homes/c1 rw,relatime shared:3921 - zfs rpool/rmng-homes-101/c1 rw,xattr,noacl
22498 22496 0:528 / /srv/rmng-homes/.merged/c1 rw,relatime shared:3951 - overlay overlay rw,lowerdir=/srv/rmng-homes/.skeleton/sha256-7,upperdir=/srv/rmng-homes/c1/upper,workdir=/srv/rmng-homes/c1/work,xino=off
22499 22496 0:529 / /clones/c1 rw,relatime - overlay overlay rw,lowerdir=/srv/rmng-homes/.skeleton/sha256-7,upperdir=/srv/rmng-homes/c1/upper,workdir=/srv/rmng-homes/c1/work
22500 22496 0:530 / /srv/rmng-homes/c10 rw,relatime - zfs rpool/rmng-homes-101/c10 rw
22501 22496 0:531 / /clones/c10 rw,relatime - overlay overlay rw,lowerdir=/x,upperdir=/srv/rmng-homes/c10/upper,workdir=/srv/rmng-homes/c10/work
22502 22496 0:532 / /mnt/with\\040space rw - zfs rpool/rmng-homes-101/c1 rw
";

    #[test]
    fn parses_a_line_with_optional_fields() {
        let m = parse_mountinfo_line(INFO.lines().next().unwrap()).unwrap();
        assert_eq!(m.mount_point, "/srv/rmng-homes");
        assert_eq!(m.fstype, "zfs");
        assert_eq!(m.source, "rpool/rmng-homes-101");
    }

    #[test]
    fn unescapes_the_mount_point() {
        assert_eq!(unescape("/mnt/with\\040space"), "/mnt/with space");
        assert_eq!(unescape("/plain"), "/plain");
        assert_eq!(unescape("/trailing\\04"), "/trailing\\04");
    }

    #[test]
    fn finds_the_dataset_and_every_overlay_on_it_but_not_a_lookalike() {
        let got = pinning_mounts(
            INFO,
            "rpool/rmng-homes-101/c1",
            Path::new("/srv/rmng-homes/c1"),
        );
        assert_eq!(
            got,
            vec![
                "/srv/rmng-homes/.merged/c1".to_string(),
                "/srv/rmng-homes/c1".to_string(),
                "/clones/c1".to_string(),
                "/mnt/with space".to_string(),
            ]
        );
        // `c10` shares a prefix with `c1` and must never match it.
        assert!(!got.iter().any(|p| p.contains("c10")));
    }

    #[test]
    fn the_homes_parent_itself_never_pins_a_clone() {
        let got = pinning_mounts(
            INFO,
            "rpool/rmng-homes-101/zz",
            Path::new("/srv/rmng-homes/zz"),
        );
        assert!(got.is_empty());
    }
}
