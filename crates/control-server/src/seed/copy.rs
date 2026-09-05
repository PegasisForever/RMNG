use anyhow::{Context, Result, bail};
use std::io::Write;
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

const BATCH_ENTRIES: usize = 4096;
const MAX_BATCH_PATHS: usize = 128;
const MAX_WORKERS: usize = 8;

struct Unit {
    path: PathBuf,
    entries: usize,
}

struct Plan {
    entries: usize,
    units: Vec<Unit>,
    directories: Vec<PathBuf>,
    hardlinks: bool,
}

// Split large directories while keeping smaller subtrees in one cp invocation.
fn plan(source: &Path, relative: &Path, limit: usize) -> Result<Plan> {
    let mut result = Plan {
        entries: 1,
        units: Vec::new(),
        directories: Vec::new(),
        hardlinks: false,
    };
    for entry in std::fs::read_dir(source.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            let child = plan(source, &path, limit)?;
            result.entries += child.entries;
            result.hardlinks |= child.hardlinks;
            result.units.extend(child.units);
            result.directories.extend(child.directories);
        } else {
            result.entries += 1;
            result.hardlinks |= metadata.nlink() > 1;
            result.units.push(Unit { path, entries: 1 });
        }
    }
    if result.entries <= limit {
        result.units = vec![Unit {
            path: relative.to_owned(),
            entries: result.entries,
        }];
        result.directories.clear();
    } else {
        result.directories.push(relative.to_owned());
    }
    Ok(result)
}

fn batches(mut units: Vec<Unit>, limit: usize) -> Vec<Vec<PathBuf>> {
    units.sort_unstable_by_key(|unit| std::cmp::Reverse(unit.entries));
    let mut result = Vec::new();
    let mut batch = Vec::new();
    let mut entries = 0;
    for unit in units {
        if !batch.is_empty() && (entries + unit.entries > limit || batch.len() == MAX_BATCH_PATHS) {
            result.push(std::mem::take(&mut batch));
            entries = 0;
        }
        entries += unit.entries;
        batch.push(unit.path);
    }
    if !batch.is_empty() {
        result.push(batch);
    }
    result
}

fn check_output(output: Output, operation: &str) -> Result<Vec<u8>> {
    if !output.status.success() {
        bail!(
            "{operation}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn with_input(mut command: Command, input: Vec<u8>) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().context("opening metadata input")?;
    // Read output while writing input so large directory lists cannot fill both pipes.
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output();
        let written = writer.join();
        let bytes = check_output(output?, "copying directory metadata")?;
        written.map_err(|_| anyhow::anyhow!("metadata writer stopped"))??;
        Ok(bytes)
    })
}

fn directory_metadata(source: &Path, directories: &[PathBuf]) -> Result<Vec<u8>> {
    let mut input = Vec::new();
    for directory in directories {
        input.extend_from_slice(directory.as_os_str().as_bytes());
        input.push(0);
    }
    let mut command = Command::new("tar");
    command.current_dir(source).args([
        "--format=pax",
        "--acls",
        "--xattrs",
        "--xattrs-include=*",
        "--no-recursion",
        "--null",
        "-T",
        "-",
        "-cf",
        "-",
    ]);
    with_input(command, input)
}

pub(super) fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_WORKERS);
    copy_with_options(source, destination, workers, BATCH_ENTRIES)?;
    Ok(())
}

fn copy_with_options(
    source: &Path,
    destination: &Path,
    workers: usize,
    limit: usize,
) -> Result<usize> {
    let started = Instant::now();
    let plan = plan(source, Path::new("."), limit).context("planning seed copy")?;
    let entries = plan.entries;
    let serial = plan.hardlinks || entries <= limit || workers <= 1;
    let batches = batches(plan.units, limit);
    let workers = if serial {
        1
    } else {
        workers.min(batches.len()).max(1)
    };
    if workers == 1 {
        // One process preserves hardlinks even when they cross directory boundaries.
        let output = Command::new("cp")
            .args(["-a", "--reflink=auto", "--"])
            .arg(source)
            .arg(destination)
            .output()
            .context("copying seed directory")?;
        check_output(output, "seed copy failed")?;
    } else {
        let metadata = directory_metadata(source, &plan.directories)?;
        std::fs::create_dir(destination)?;
        // Workers share these ancestors. Create them before any worker starts.
        for directory in &plan.directories {
            std::fs::create_dir_all(destination.join(directory))?;
        }
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        std::thread::scope(|scope| -> Result<()> {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| -> Result<()> {
                        while !failed.load(Ordering::Relaxed) {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(batch) = batches.get(index) else {
                                break;
                            };
                            let output = Command::new("cp")
                                .current_dir(source)
                                .args(["-a", "--parents", "--reflink=auto", "--"])
                                .args(batch)
                                .arg(destination)
                                .output()
                                .context("starting seed copy worker")
                                .and_then(|output| check_output(output, "seed copy worker failed"));
                            if let Err(error) = output {
                                failed.store(true, Ordering::Relaxed);
                                return Err(error);
                            }
                        }
                        Ok(())
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|handle| handle.join()).collect();
            for result in results {
                result.map_err(|_| anyhow::anyhow!("seed copy worker stopped"))??;
            }
            Ok(())
        })?;
        // cp --parents omits ancestor attributes. Restore them after all child writes.
        let mut restore = Command::new("tar");
        restore
            .args([
                "--acls",
                "--xattrs",
                "--xattrs-include=*",
                "--numeric-owner",
                "--same-owner",
                "-C",
            ])
            .arg(destination)
            .args(["-xpf", "-"]);
        with_input(restore, metadata)?;
    }
    tracing::info!(path = %source.display(), entries, workers, seconds = started.elapsed().as_secs_f64(), "seed directory copied");
    Ok(workers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CString, OsString};
    use std::os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    };
    use std::time::{Duration, SystemTime};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "rmng-copy-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn assert_tree(source: &Path, destination: &Path) {
        let a = std::fs::symlink_metadata(source).unwrap();
        let b = std::fs::symlink_metadata(destination).unwrap();
        assert_eq!(
            (a.mode(), a.uid(), a.gid(), a.mtime(), a.mtime_nsec()),
            (b.mode(), b.uid(), b.gid(), b.mtime(), b.mtime_nsec()),
            "{}",
            source.display()
        );
        if a.is_dir() {
            let mut a: Vec<_> = std::fs::read_dir(source)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            let mut b: Vec<_> = std::fs::read_dir(destination)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            a.sort();
            b.sort();
            assert_eq!(a, b);
            for name in a {
                assert_tree(&source.join(&name), &destination.join(&name));
            }
        } else if a.is_symlink() {
            assert_eq!(
                std::fs::read_link(source).unwrap(),
                std::fs::read_link(destination).unwrap()
            );
        } else {
            assert_eq!(
                std::fs::read(source).unwrap(),
                std::fs::read(destination).unwrap()
            );
        }
    }

    #[test]
    fn parallel_copy_preserves_metadata_and_unusual_names() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        std::fs::create_dir(&source).unwrap();
        let directories = [".", "-option", "line\nbreak", "nested/sub", "empty"];
        for directory in directories {
            std::fs::create_dir_all(source.join(directory)).unwrap();
        }
        for index in 0..40 {
            std::fs::write(
                source.join(format!("nested/sub/file{index}")),
                format!("dirty {index}"),
            )
            .unwrap();
        }
        std::fs::write(
            source
                .join("-option")
                .join(OsString::from_vec(vec![b'f', 255])),
            b"bytes",
        )
        .unwrap();
        symlink("missing", source.join("broken")).unwrap();
        symlink("cycle-b", source.join("cycle-a")).unwrap();
        symlink("cycle-a", source.join("cycle-b")).unwrap();
        let timestamp = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        for directory in [
            ".",
            "-option",
            "line\nbreak",
            "nested",
            "nested/sub",
            "empty",
        ] {
            let path = source.join(directory);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o2750)).unwrap();
            std::fs::File::open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(timestamp))
                .unwrap();
            let path = CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::setxattr(
                        path.as_ptr(),
                        c"user.seed".as_ptr(),
                        b"keep".as_ptr().cast(),
                        4,
                        0,
                    )
                },
                0
            );
        }
        assert_eq!(copy_with_options(&source, &destination, 4, 3).unwrap(), 4);
        assert_tree(&source, &destination);
        for directory in [".", "nested", "nested/sub", "empty"] {
            let path = CString::new(destination.join(directory).as_os_str().as_bytes()).unwrap();
            let mut value = [0u8; 4];
            assert_eq!(
                unsafe {
                    libc::getxattr(
                        path.as_ptr(),
                        c"user.seed".as_ptr(),
                        value.as_mut_ptr().cast(),
                        4,
                    )
                },
                4
            );
            assert_eq!(&value, b"keep");
        }
    }

    #[test]
    fn hardlinks_use_one_worker_and_remain_independent_of_source() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        std::fs::create_dir_all(source.join("a")).unwrap();
        std::fs::create_dir_all(source.join("b")).unwrap();
        std::fs::write(source.join("a/file"), b"original").unwrap();
        std::fs::hard_link(source.join("a/file"), source.join("b/file")).unwrap();
        symlink("missing", source.join("a/link")).unwrap();
        std::fs::hard_link(source.join("a/link"), source.join("b/link")).unwrap();
        assert_eq!(copy_with_options(&source, &destination, 4, 2).unwrap(), 1);
        assert_tree(&source, &destination);
        for name in ["file", "link"] {
            let a = std::fs::symlink_metadata(destination.join("a").join(name)).unwrap();
            let b = std::fs::symlink_metadata(destination.join("b").join(name)).unwrap();
            assert_eq!(a.ino(), b.ino());
        }
        std::fs::write(destination.join("a/file"), b"child edit").unwrap();
        assert_eq!(std::fs::read(source.join("a/file")).unwrap(), b"original");
        assert_eq!(
            std::fs::read(destination.join("b/file")).unwrap(),
            b"child edit"
        );
    }

    #[test]
    fn parallel_copy_survives_removed_home_links() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        std::fs::create_dir_all(source.join("project")).unwrap();
        std::fs::create_dir(&destination).unwrap();
        for index in 0..20 {
            std::fs::write(source.join(format!("project/file{index}")), b"copy").unwrap();
        }
        let link = fixture.0.join("source-link");
        symlink(&source, &link).unwrap();
        let source_fd = std::fs::File::open(&link).unwrap();
        let destination_fd = std::fs::File::open(&destination).unwrap();
        std::fs::remove_file(&link).unwrap();
        assert_eq!(
            copy_with_options(
                &super::super::pinned_home(&source_fd).join("project"),
                &super::super::pinned_home(&destination_fd).join("project"),
                4,
                3,
            )
            .unwrap(),
            4
        );
        assert_tree(&source.join("project"), &destination.join("project"));
    }
}
