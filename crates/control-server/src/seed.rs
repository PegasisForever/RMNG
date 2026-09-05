use crate::app::App;
use anyhow::{Context, Result, bail};
use std::path::{Component, Path};

mod copy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSpec {
    pub source: String,
    pub paths: Vec<String>,
}

pub fn validate_paths(paths: &[String]) -> Result<()> {
    if paths.is_empty() || paths.len() > 16 {
        bail!("seed requires between one and sixteen directories");
    }
    for (index, path) in paths.iter().enumerate() {
        let rel = path
            .strip_prefix("/home/rmng/")
            .context("seed paths must be below /home/rmng")?;
        if rel.is_empty()
            || rel.contains('\0')
            || Path::new(rel)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("invalid seed directory: {path}");
        }
        for other in &paths[..index] {
            if Path::new(path).starts_with(other) || Path::new(other).starts_with(path) {
                bail!("seed directories cannot overlap: {path} and {other}");
            }
        }
    }
    Ok(())
}

pub fn parse(
    app: &App,
    value: Option<&serde_json::Value>,
    caller: Option<String>,
) -> Result<Option<SeedSpec>> {
    let Some(value) = value else { return Ok(None) };
    let paths: Vec<String> = serde_json::from_value(value.clone())
        .context("seed must be an array of directory paths")?;
    validate_paths(&paths)?;
    let source = caller.context("seed requires a request from a managed clone")?;
    let state = app.store.get();
    let host = state
        .hosts
        .iter()
        .find(|host| host.id == source && host.managed && !host.archived)
        .context("seed source must be a running managed clone")?;
    Ok(Some(SeedSpec {
        source: host.id.clone(),
        paths,
    }))
}

pub async fn copy(app: &App, seed: &SeedSpec, destination: &str) -> Result<()> {
    crate::homes::ensure_now(app, &seed.source).await;
    crate::homes::ensure_now(app, destination).await;
    let data_dir = app.config().data_dir;
    let source_home = crate::homes::host_path(&data_dir, &seed.source, "/home/rmng")
        .context("invalid source clone")?;
    let destination_home = crate::homes::host_path(&data_dir, destination, "/home/rmng")
        .context("invalid destination clone")?;
    let paths = seed.paths.clone();
    tokio::task::spawn_blocking(move || {
        // Open directories survive browse-link changes during clone registration.
        let source = std::fs::File::open(&source_home).context("opening source home")?;
        let destination =
            std::fs::File::open(&destination_home).context("opening destination home")?;
        seed_directories(&pinned_home(&source), &pinned_home(&destination), &paths)
    })
    .await?
}

fn pinned_home(file: &std::fs::File) -> std::path::PathBuf {
    use std::os::fd::AsRawFd;
    std::path::PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
}

fn seed_directories(source_home: &Path, destination_home: &Path, paths: &[String]) -> Result<()> {
    validate_paths(paths)?;
    for (index, path) in paths.iter().enumerate() {
        let relative = Path::new(path.strip_prefix("/home/rmng/").unwrap());
        check_parents(source_home, relative)?;
        check_parents(destination_home, relative.parent().unwrap_or(Path::new("")))?;
        let source = source_home.join(relative);
        if !source.is_dir() {
            bail!("seed source is not a directory: {path}");
        }
        let destination = destination_home.join(relative);
        let parent = destination
            .parent()
            .context("seed directory has no parent")?;
        std::fs::create_dir_all(parent)?;
        let staging = destination_home.join(format!(".rmng-seed-{index}"));
        let backup = destination_home.join(format!(".rmng-seed-backup-{index}"));
        if staging.symlink_metadata().is_ok() || backup.symlink_metadata().is_ok() {
            bail!("seed staging path already exists");
        }
        copy::copy_directory(&source, &staging)?;
        let had_destination = destination.symlink_metadata().is_ok();
        if had_destination {
            std::fs::rename(&destination, &backup)?;
        }
        if let Err(error) = std::fs::rename(&staging, &destination) {
            if had_destination {
                let _ = std::fs::rename(&backup, &destination);
            }
            return Err(error.into());
        }
        // New parent directories must belong to the clone user.
        use std::os::unix::fs::MetadataExt;
        let owner = std::fs::metadata(&source)?;
        let mut directory = parent.to_path_buf();
        while directory != destination_home && directory.starts_with(destination_home) {
            std::os::unix::fs::chown(&directory, Some(owner.uid()), Some(owner.gid()))?;
            directory.pop();
        }
    }
    Ok(())
}

fn check_parents(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("seed path traverses a symbolic link: {}", path.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn seed_rejects_home_escape_and_overlap() {
        for paths in [
            vec!["/home/rmng"],
            vec!["/home/rmng/../root"],
            vec!["/home/rmngx/project"],
            vec!["/home/rmng/a", "/home/rmng/a/b"],
        ] {
            assert!(
                validate_paths(&paths.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
            );
        }
    }

    #[test]
    fn seed_survives_removed_browse_links() {
        let root = std::env::temp_dir().join(format!("rmng-seed-pin-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("source/project")).unwrap();
        std::fs::create_dir_all(root.join("destination")).unwrap();
        std::fs::write(root.join("source/project/file"), "dirty").unwrap();
        symlink(root.join("source"), root.join("source-link")).unwrap();
        symlink(root.join("destination"), root.join("destination-link")).unwrap();
        let source = std::fs::File::open(root.join("source-link")).unwrap();
        let destination = std::fs::File::open(root.join("destination-link")).unwrap();
        std::fs::remove_file(root.join("source-link")).unwrap();
        std::fs::remove_file(root.join("destination-link")).unwrap();
        seed_directories(
            &pinned_home(&source),
            &pinned_home(&destination),
            &["/home/rmng/project".into()],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("destination/project/file")).unwrap(),
            "dirty"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn seed_preserves_links_git_and_uncommitted_files() {
        let root = std::env::temp_dir().join(format!("rmng-seed-test-{}", std::process::id()));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(source.join("project/.git")).unwrap();
        std::fs::create_dir_all(destination.join("project/link")).unwrap();
        std::fs::write(source.join("project/.git/HEAD"), "ref: refs/heads/main").unwrap();
        std::fs::write(source.join("project/dirty"), "uncommitted").unwrap();
        symlink("missing", source.join("project/link")).unwrap();
        symlink("cycle-b", source.join("project/cycle-a")).unwrap();
        symlink("cycle-a", source.join("project/cycle-b")).unwrap();
        std::fs::write(destination.join("project/stale"), "old").unwrap();
        seed_directories(&source, &destination, &["/home/rmng/project".into()]).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("project/dirty")).unwrap(),
            "uncommitted"
        );
        assert!(destination.join("project/.git/HEAD").is_file());
        assert_eq!(
            std::fs::read_link(destination.join("project/link")).unwrap(),
            Path::new("missing")
        );
        assert_eq!(
            std::fs::read_link(destination.join("project/cycle-a")).unwrap(),
            Path::new("cycle-b")
        );
        assert!(!destination.join("project/stale").exists());
        assert!(destination.join(".rmng-seed-backup-0/stale").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }
}
