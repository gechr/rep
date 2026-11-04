//! Commit one replacement without exposing the original to a partial write.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use tempfile::NamedTempFile;

/// Atomically replace the named file after verifying its expected contents.
/// Symlinks retain their target relationship. Other hard links retain the old
/// inode. This is a per-file replacement, not a power-loss durability promise.
pub(crate) fn commit(path: &Path, expected: &[u8], updated: &[u8]) -> Result<()> {
    commit_with(path, expected, updated, write_contents, persist_temp)
        .with_context(|| format!("Unable to write to {path:?}"))
}

fn persist_temp(temp: NamedTempFile, target: &Path) -> Result<()> {
    // PersistError owns the temporary file. Keep only the I/O error so a
    // caller retaining diagnostics cannot also retain a staged replacement.
    temp.persist(target).map_err(|failure| failure.error)?;
    Ok(())
}

fn write_contents(file: &mut File, updated: &[u8]) -> io::Result<()> {
    file.write_all(updated)?;
    file.flush()
}

/// The two operations are supplied separately so tests can fail either side
/// of the commit boundary without filesystem-specific fault injection.
fn commit_with(
    path: &Path,
    expected: &[u8],
    updated: &[u8],
    stage: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
    persist: impl FnOnce(NamedTempFile, &Path) -> Result<()>,
) -> Result<()> {
    let target = fs::canonicalize(path)?;
    // Opening for writing checks effective permissions without truncating.
    // In particular, writable parent directories must not make read-only
    // files writable merely because rename would be allowed.
    let mut source = OpenOptions::new().read(true).write(true).open(&target)?;
    let original = source.metadata()?;
    ensure!(original.is_file(), "destination is not a regular file");
    verify_contents(&mut source, expected)?;
    drop(source);

    let parent = target
        .parent()
        .context("destination has no parent directory")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".rep-")
        .tempfile_in(parent)?;
    stage(temp.as_file_mut(), updated)?;
    preserve_metadata(temp.as_file(), &original)?;

    // Reopen the name, not the old handle: an editor may have replaced the
    // directory entry while the replacement was being prepared.
    ensure!(
        fs::canonicalize(path)? == target,
        "destination changed since it was read"
    );
    let mut current = File::open(&target)?;
    ensure!(
        same_version(&original, &current.metadata()?),
        "destination changed since it was read"
    );
    verify_contents(&mut current, expected)?;
    drop(current);
    // There is necessarily a race with unrelated writers after this check.
    // We detect observable conflicts, without claiming a filesystem CAS.
    persist(temp, &target)
}

fn verify_contents(file: &mut File, expected: &[u8]) -> Result<()> {
    let mut buffer = [0_u8; 8192];
    for chunk in expected.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..chunk.len()])
            .context("destination changed since it was read")?;
        ensure!(
            &buffer[..chunk.len()] == chunk,
            "destination changed since it was read"
        );
    }
    ensure!(
        file.read(&mut buffer[..1])? == 0,
        "destination changed since it was read"
    );
    Ok(())
}

fn preserve_metadata(file: &File, original: &Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, fchown};

        let actual = file.metadata()?;
        if actual.uid() != original.uid() || actual.gid() != original.gid() {
            fchown(file, Some(original.uid()), Some(original.gid()))?;
        }
    }
    // Ownership changes can clear permission bits, so copy permissions last.
    file.set_permissions(original.permissions())
}

fn same_version(before: &Metadata, after: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        // Replacing another hard link changes ctime without changing this
        // destination's version. Identity, content, mtime and access metadata
        // still detect edits relevant to the replacement.
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
            || before.uid() != after.uid()
            || before.gid() != after.gid()
        {
            return false;
        }
    }
    before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && before.permissions() == after.permissions()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.txt");
        fs::write(&path, b"original\n").unwrap();
        (dir, path)
    }

    fn assert_untouched(dir: &tempfile::TempDir, path: &Path) {
        assert_eq!(fs::read(path).unwrap(), b"original\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn partial_staging_failure_preserves_original_and_cleans_temporary() {
        let (dir, path) = fixture();
        let result = commit_with(
            &path,
            b"original\n",
            b"replacement\n",
            |file, bytes| {
                file.write_all(&bytes[..3])?;
                Err(io::Error::other("injected partial write failure"))
            },
            |_, _| panic!("failed staging must never persist"),
        );
        assert!(result.is_err());
        assert_untouched(&dir, &path);
    }

    #[test]
    fn flush_failure_preserves_original_and_cleans_temporary() {
        let (dir, path) = fixture();
        let result = commit_with(
            &path,
            b"original\n",
            b"replacement\n",
            |file, bytes| {
                file.write_all(bytes)?;
                Err(io::Error::other("injected flush failure"))
            },
            |_, _| panic!("failed flush must never persist"),
        );
        assert!(result.is_err());
        assert_untouched(&dir, &path);
    }

    #[test]
    fn persist_failure_preserves_original_and_cleans_temporary() {
        let (dir, path) = fixture();
        let result = commit_with(
            &path,
            b"original\n",
            b"replacement\n",
            write_contents,
            |_, _| Err(io::Error::other("injected persist failure").into()),
        );
        assert!(result.is_err());
        assert_untouched(&dir, &path);
    }

    #[test]
    fn real_persist_error_does_not_retain_temporary_file() {
        let (dir, path) = fixture();
        let result = commit_with(
            &path,
            b"original\n",
            b"replacement\n",
            write_contents,
            |temp, _| persist_temp(temp, dir.path()),
        );
        assert!(result.is_err());
        // Check cleanup while the returned error is still alive.
        assert_untouched(&dir, &path);
    }

    #[test]
    fn stale_contents_are_rejected() {
        let (dir, path) = fixture();
        assert!(commit(&path, b"stale\n", b"replacement\n").is_err());
        assert_untouched(&dir, &path);
    }

    #[test]
    fn edit_during_staging_is_preserved() {
        let (dir, path) = fixture();
        let result = commit_with(
            &path,
            b"original\n",
            b"replacement\n",
            |file, bytes| {
                write_contents(file, bytes)?;
                fs::write(&path, b"editor change\n")
            },
            |_, _| panic!("conflicting edit must never persist"),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"editor change\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn replaced_inode_with_identical_contents_is_rejected() {
        let (dir, path) = fixture();
        let result = commit_with(
            &path,
            b"original\n",
            b"replacement\n",
            |file, bytes| {
                write_contents(file, bytes)?;
                let other = dir.path().join("editor.txt");
                fs::write(&other, b"original\n")?;
                fs::rename(other, &path)
            },
            |_, _| panic!("replaced inode must never persist"),
        );
        assert!(result.is_err());
        assert_untouched(&dir, &path);
    }

    #[test]
    fn successful_commit_can_empty_file() {
        let (dir, path) = fixture();
        commit(&path, b"original\n", b"").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn successful_commit_preserves_permissions_and_ownership() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let (_dir, path) = fixture();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o751)).unwrap();
        let before = fs::metadata(&path).unwrap();
        commit(&path, b"original\n", b"replacement\n").unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.mode(), before.mode());
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.gid(), before.gid());
        assert_ne!(after.ino(), before.ino());
        assert_eq!(fs::read(&path).unwrap(), b"replacement\n");
    }

    #[cfg(unix)]
    #[test]
    fn writable_parent_does_not_allow_rewriting_readonly_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let (dir, path) = fixture();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        // A privileged runner may legitimately have effective write access.
        if OpenOptions::new().write(true).open(&path).is_ok() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            return;
        }
        let result = commit(&path, b"original\n", b"replacement\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.is_err());
        assert_untouched(&dir, &path);
    }

    #[cfg(unix)]
    #[test]
    fn commit_through_symlink_keeps_link() {
        use std::os::unix::fs::symlink;

        let (dir, path) = fixture();
        let link = dir.path().join("link.txt");
        symlink("data.txt", &link).unwrap();
        commit(&link, b"original\n", b"replacement\n").unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("data.txt"));
        assert_eq!(fs::read(path).unwrap(), b"replacement\n");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_retargeted_during_staging_is_rejected() {
        use std::os::unix::fs::symlink;

        let (dir, path) = fixture();
        let other = dir.path().join("other.txt");
        let link = dir.path().join("link.txt");
        fs::write(&other, b"other\n").unwrap();
        symlink(&path, &link).unwrap();
        let result = commit_with(
            &link,
            b"original\n",
            b"replacement\n",
            |file, bytes| {
                write_contents(file, bytes)?;
                fs::remove_file(&link)?;
                symlink(&other, &link)
            },
            |_, _| panic!("retargeted symlink must never persist"),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(path).unwrap(), b"original\n");
        assert_eq!(fs::read(other).unwrap(), b"other\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn commit_breaks_only_named_hard_link() {
        let (dir, path) = fixture();
        let other = dir.path().join("other.txt");
        fs::hard_link(&path, &other).unwrap();
        commit(&path, b"original\n", b"replacement\n").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"replacement\n");
        assert_eq!(fs::read(other).unwrap(), b"original\n");
    }

    #[test]
    fn committing_another_hard_link_during_staging_is_not_a_conflict() {
        let (dir, path) = fixture();
        let other = dir.path().join("other.txt");
        fs::hard_link(&path, &other).unwrap();
        commit_with(
            &path,
            b"original\n",
            b"first replacement\n",
            |file, bytes| {
                write_contents(file, bytes)?;
                commit(&other, b"original\n", b"second replacement\n").map_err(io::Error::other)
            },
            persist_temp,
        )
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), b"first replacement\n");
        assert_eq!(fs::read(other).unwrap(), b"second replacement\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
