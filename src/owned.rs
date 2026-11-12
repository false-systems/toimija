//! The one guard for what Toimija writes, appends, and deletes.
//!
//! A repository can commit `.toimija`, any directory under it, or any file
//! Toimija writes there, as a symlink. Following one would let whoever
//! committed it point Toimija's writes and deletions anywhere the user can
//! write. So, under `.toimija/`:
//!
//! - every directory Toimija writes into, or deletes from, is checked one
//!   component at a time from the repository root down: each must be a real
//!   directory, never a symlink, and missing ones are created one at a time;
//! - every file opened for writing or appending is opened with `O_NOFOLLOW`,
//!   so a symlink there is refused, not followed;
//! - atomic replacement renames onto the target, which replaces a symlink
//!   instead of writing through it. The bytes are staged in `.toimija/tmp/`,
//!   a real directory checked the same way, whose own `.gitignore` hides
//!   everything in it from Git: a reader that lists untracked files and then
//!   stats them (`git add --all`, so Sykli) never meets a temp file that the
//!   rename is about to take away.
//!
//! Toimija writes no file outside `.toimija/`: the human-owned files at the
//! repository root (AGENTS.md, CLAUDE.md, hooks, settings) are never touched.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The directory Toimija owns, relative to the repository root.
pub const TOIMIJA_DIR: &str = ".toimija";

/// Where atomic replacements stage their bytes, relative to the repository
/// root. It ignores itself: see [`STAGING_IGNORE`].
pub const STAGING_DIR: &str = ".toimija/tmp";

/// The `.gitignore` written inside [`STAGING_DIR`]. `*` matches every entry,
/// this file included, so Git lists nothing there whatever the repository's
/// own ignore rules say.
const STAGING_IGNORE: &[u8] = b"*\n";

/// Make sure `dir` — `.toimija` or a directory beneath it — exists as a real
/// directory, creating missing components one at a time. Refuses a component
/// that is a symlink or not a directory.
pub fn ensure_dir(repo_root: &Path, dir: &Path) -> Result<()> {
    walk(repo_root, dir, true).map(|_| ())
}

/// Whether `dir` — `.toimija` or a directory beneath it — exists as a real
/// directory. `Ok(false)` when some component is absent; an error when one is
/// a symlink or not a directory.
pub fn dir_exists(repo_root: &Path, dir: &Path) -> Result<bool> {
    walk(repo_root, dir, false)
}

fn walk(repo_root: &Path, dir: &Path, create: bool) -> Result<bool> {
    let relative = dir.strip_prefix(repo_root).with_context(|| {
        format!(
            "refusing to use {}: it is not inside {}",
            dir.display(),
            repo_root.display()
        )
    })?;
    let mut components = relative.components();
    if components.next() != Some(Component::Normal(OsStr::new(TOIMIJA_DIR))) {
        bail!(
            "refusing to use {}: Toimija writes only under {TOIMIJA_DIR}/",
            relative.display()
        );
    }
    let mut current = repo_root.join(TOIMIJA_DIR);
    let mut rest = components.peekable();
    loop {
        if !real_dir(repo_root, &current, create)? {
            return Ok(false);
        }
        match rest.next() {
            None => return Ok(true),
            Some(Component::Normal(name)) => current.push(name),
            Some(_) => bail!(
                "refusing to use {}: not a plain path under {TOIMIJA_DIR}/",
                relative.display()
            ),
        }
    }
}

/// Whether `path` is a real directory; with `create`, make it one when it is
/// missing. Never follows a symlink.
fn real_dir(repo_root: &Path, path: &Path, create: bool) -> Result<bool> {
    let refuse = |what: &str| -> Result<bool> {
        bail!(
            "refusing to use {}: it is {what}; Toimija writes and deletes only inside real \
             directories under {TOIMIJA_DIR}/",
            display(repo_root, path)
        )
    };
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => refuse("a symlink"),
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => refuse("not a directory"),
        Err(error) if error.kind() == io::ErrorKind::NotFound && !create => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => Ok(true),
            // A concurrent writer created it first: check what it made.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                real_dir(repo_root, path, false)
            }
            Err(error) => {
                Err(error).with_context(|| format!("failed to create {}", path.display()))
            }
        },
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

/// Atomically replace `path`, a file under `.toimija/`, with `contents`. Its
/// directory must already exist as a real one ([`ensure_dir`] creates it). A
/// symlink at `path` is replaced, never written through. The bytes are staged
/// in [`STAGING_DIR`], which Git never lists.
pub fn replace(repo_root: &Path, path: &Path, contents: &[u8]) -> Result<()> {
    replace_staged(repo_root, path, contents, |_| Ok(()))
}

fn replace_staged(
    repo_root: &Path,
    path: &Path,
    contents: &[u8],
    before_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<()> {
    require_parent(repo_root, path)?;
    let staging =
        staging_dir(repo_root).with_context(|| format!("failed to write {}", path.display()))?;
    crate::atomic_write::replace_before_rename(path, &staging, contents, before_rename)
        .map_err(anyhow::Error::from)
        .with_context(|| {
            format!(
                "failed to write {} (staged in {})",
                path.display(),
                staging.display()
            )
        })
}

/// The staging directory, made a real directory and given its `.gitignore`
/// before any temp file is created in it — so Git never lists one. An
/// existing `.gitignore` that says anything but `*` (emptied by a crash
/// between create and write, or by hand) is rewritten in place. A symlink
/// there is refused, never followed.
fn staging_dir(repo_root: &Path) -> Result<PathBuf> {
    use std::io::{Read as _, Write as _};
    let staging = repo_root.join(STAGING_DIR);
    ensure_dir(repo_root, &staging)?;
    let ignore = staging.join(".gitignore");
    let write = |options: &mut OpenOptions| -> Result<()> {
        let mut file = open_nofollow(&ignore, options)
            .map_err(|error| refusal_or(repo_root, &ignore, error, "write"))?;
        file.write_all(STAGING_IGNORE)
            .with_context(|| format!("failed to write {}", ignore.display()))
    };
    match write(OpenOptions::new().write(true).create_new(true)) {
        Ok(()) => {}
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io| io.kind() == io::ErrorKind::AlreadyExists) =>
        {
            let mut current = Vec::new();
            open_nofollow(&ignore, OpenOptions::new().read(true))
                .and_then(|mut file| file.read_to_end(&mut current))
                .map_err(|error| refusal_or(repo_root, &ignore, error, "read"))?;
            if current != STAGING_IGNORE {
                write(OpenOptions::new().write(true).truncate(true))?;
            }
        }
        Err(error) => return Err(error),
    }
    Ok(staging)
}

/// Remove `dir`, a directory under `.toimija/`, and everything in it. Already
/// gone is fine; a symlink on the way to it, or at it, is refused, never
/// followed. Inside it, a symlink is removed as a link, not followed.
pub fn remove_dir(repo_root: &Path, dir: &Path) -> Result<()> {
    if !dir_exists(repo_root, dir)? {
        return Ok(());
    }
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", dir.display())),
    }
}

fn require_parent(repo_root: &Path, path: &Path) -> Result<()> {
    let parent = path.parent().context("a file has a parent")?;
    if dir_exists(repo_root, parent)? {
        Ok(())
    } else {
        Err(anyhow::Error::from(io::Error::from(
            io::ErrorKind::NotFound,
        )))
        .with_context(|| format!("{} does not exist", display(repo_root, parent)))
    }
}

/// Open a file without following a symlink at its last component.
pub fn open_nofollow(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
        options.open(path)
    }
    #[cfg(not(unix))]
    {
        if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(io::Error::other("refusing to open a symlink"));
        }
        options.open(path)
    }
}

fn refusal_or(repo_root: &Path, path: &Path, error: io::Error, verb: &str) -> anyhow::Error {
    let is_link = fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    if is_link {
        anyhow::anyhow!(
            "refusing to {verb} {}: it is a symlink; Toimija never writes through one",
            display(repo_root, path)
        )
    } else {
        anyhow::Error::from(error).context(format!("failed to {verb} {}", path.display()))
    }
}

fn display(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_symlink_anywhere_on_the_way_is_refused_and_nothing_is_created_behind_it() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(repo.join(".toimija")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".toimija/sessions")).unwrap();

        let dir = repo.join(".toimija/sessions/100-1");
        let error = ensure_dir(&repo, &dir).unwrap_err().to_string();
        assert!(error.contains("symlink"), "{error}");
        assert!(!outside.join("100-1").exists());
        assert!(dir_exists(&repo, &dir).is_err());
        assert!(ensure_dir(&repo, &repo.join(".toimija/tmp")).is_ok());
        assert!(dir_exists(&repo, &repo.join(".toimija/tmp")).unwrap());
        assert!(!dir_exists(&repo, &repo.join(".toimija/other")).unwrap());
    }

    #[test]
    fn removing_a_directory_follows_no_symlink_on_the_way_or_inside() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "keep\n").unwrap();

        // A symlink inside the directory is removed as a link.
        let session = repo.join(".toimija/sessions/100-1");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("session.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside, session.join("link")).unwrap();
        remove_dir(&repo, &session).unwrap();
        assert!(!session.exists());
        assert!(outside.join("keep").exists());
        // Gone already is fine.
        remove_dir(&repo, &session).unwrap();

        // A symlink on the way is refused.
        fs::remove_dir_all(repo.join(".toimija/sessions")).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".toimija/sessions")).unwrap();
        let error = remove_dir(&repo, &repo.join(".toimija/sessions/keep")).unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn only_paths_under_toimija_are_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        assert!(ensure_dir(repo, &repo.join("src")).is_err());
        assert!(ensure_dir(repo, &repo.join(".toimija/../src")).is_err());
        assert!(ensure_dir(repo, Path::new("/elsewhere/.toimija")).is_err());
    }

    #[test]
    fn a_file_symlink_is_replaced_not_written_through_and_a_missing_directory_reads_as_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".toimija")).unwrap();
        let victim = temp.path().join("victim");
        fs::write(&victim, "keep\n").unwrap();
        let link = repo.join(".toimija/handover.json");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        replace(&repo, &link, b"{}\n").unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep\n");
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&link).unwrap(), "{}\n");

        let missing = replace(
            &repo,
            &repo.join(".toimija/sessions/100-1/session.json"),
            b"{}\n",
        )
        .unwrap_err();
        assert!(
            missing
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
        );
    }
}

#[cfg(all(test, unix))]
mod staging_tests {
    use super::*;
    use crate::command::{CommandRunner, ProcessCommandRunner};

    /// Sykli (`git add --all` on a scratch index) and every other Git reader
    /// list untracked files, then stat them. A temp file Git can list
    /// vanishes under it at the rename: `fatal: unable to stat
    /// '.toimija/handover.json.tmp-…'`.
    #[test]
    fn git_never_lists_a_replacement_in_flight() {
        let runner = ProcessCommandRunner;
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        if runner
            .run(repo, "git", &["init", "-q"])
            .map_or(true, |output| output.status != 0)
        {
            return; // no usable git here
        }
        ensure_dir(repo, &repo.join(".toimija/sessions/100-1")).unwrap();
        for target in [
            repo.join(".toimija/handover.json"),
            repo.join(".toimija/sessions/100-1/session.json"),
        ] {
            let mut listed = String::new();
            let mut staged_at = PathBuf::new();
            replace_staged(repo, &target, b"{}\n", |temp_file| {
                staged_at = temp_file.to_path_buf();
                let output = runner
                    .run(
                        repo,
                        "git",
                        &["ls-files", "--others", "--exclude-standard", "-z"],
                    )
                    .unwrap();
                listed = output.stdout_lossy().into_owned();
                Ok(())
            })
            .unwrap();
            assert!(
                staged_at.starts_with(repo.join(STAGING_DIR)),
                "{}",
                staged_at.display()
            );
            assert!(
                !listed.contains(crate::atomic_write::TEMP_MARKER),
                "git listed a temp file it would then fail to stat: {listed:?}"
            );
            assert!(!listed.contains(".toimija/tmp"), "{listed:?}");
            assert_eq!(fs::read(&target).unwrap(), b"{}\n");
        }
        assert_eq!(
            fs::read(repo.join(STAGING_DIR).join(".gitignore")).unwrap(),
            STAGING_IGNORE
        );
    }

    #[test]
    fn an_emptied_staging_ignore_file_is_repaired_before_the_next_write() {
        // A crash or a full disk between creating the file and writing it, or
        // a hand edit, leaves it empty: Git would list temp files again.
        let runner = ProcessCommandRunner;
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        if runner
            .run(repo, "git", &["init", "-q"])
            .map_or(true, |output| output.status != 0)
        {
            return;
        }
        ensure_dir(repo, &repo.join(STAGING_DIR)).unwrap();
        let ignore = repo.join(STAGING_DIR).join(".gitignore");
        fs::write(&ignore, b"").unwrap();

        let mut listed = String::new();
        replace_staged(repo, &repo.join(".toimija/handover.json"), b"x", |_| {
            listed = runner
                .run(repo, "git", &["ls-files", "--others", "--exclude-standard"])
                .unwrap()
                .stdout_lossy()
                .into_owned();
            Ok(())
        })
        .unwrap();

        assert_eq!(fs::read(&ignore).unwrap(), STAGING_IGNORE);
        assert!(!listed.contains(".toimija/tmp"), "{listed:?}");
    }

    #[test]
    fn a_symlinked_staging_directory_is_refused_and_nothing_lands_behind_it() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(repo.join(".toimija")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(STAGING_DIR)).unwrap();

        let error = format!(
            "{:#}",
            replace(&repo, &repo.join(".toimija/handover.json"), b"{}").unwrap_err()
        );

        assert!(error.contains("symlink"), "{error}");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert!(!repo.join(".toimija/handover.json").exists());
    }
}
