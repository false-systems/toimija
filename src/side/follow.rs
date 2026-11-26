//! Where the shell beside the pane is working, so the pane can show that
//! repository without being asked.
//!
//! The pane is opened from a shell and is told that shell's process. The
//! shell's current directory is asked of the kernel, which starts nothing,
//! and the repository is read from the `.git` found above that directory.

use std::path::{Path, PathBuf};

/// The current directory of the process `pid`, when the kernel says.
#[cfg(target_os = "linux")]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The current directory of the process `pid`, when the kernel says.
#[cfg(target_os = "macos")]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    let pid = libc::c_int::try_from(pid).ok()?;
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
    // SAFETY: the struct is plain data the kernel fills; zeroed is a valid
    // start, and the size passed is the size of what the pointer points at.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` lives for the call and is `size` bytes long.
    let filled = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            std::ptr::addr_of_mut!(info).cast(),
            libc::c_int::try_from(size).ok()?,
        )
    };
    if usize::try_from(filled).ok()? < size {
        return None;
    }
    // The path is 1024 bytes, declared as 32 rows of 32 for old compilers.
    let rows = &info.pvi_cdir.vip_path;
    // SAFETY: `rows` is 32 * 32 contiguous bytes inside `info`.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(rows.as_ptr().cast::<u8>(), 32 * 32) };
    let end = bytes.iter().position(|&byte| byte == 0)?;
    if end == 0 {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd_of(_pid: u32) -> Option<PathBuf> {
    None
}

/// The repository of the workspace a directory is in, and the branch
/// checked out there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Here {
    /// The repository's folder name in the workspace.
    pub repository: String,
    /// The top of the working copy the shell is in.
    pub top: PathBuf,
    /// The branch checked out where the shell is: in a linked worktree,
    /// that worktree's branch. `None` when HEAD is not on a branch.
    pub branch: Option<String>,
}

/// Which repository of the workspace at `root` the directory `cwd` belongs
/// to: the one it is inside, or the one whose linked worktree it is inside.
/// Read from files; no git is run.
pub fn here(root: &Path, cwd: &Path) -> Option<Here> {
    let (top, dot_git) = cwd
        .ancestors()
        .map(|dir| (dir, dir.join(".git")))
        .find(|(_, dot_git)| dot_git.exists())?;
    // A linked worktree's `.git` is a file naming its own git directory,
    // which names the repository's in `commondir`.
    let (git_dir, main_top) = if dot_git.is_dir() {
        (dot_git.clone(), top.to_path_buf())
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let named = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        let git_dir = top.join(named);
        let common = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
        let common = git_dir.join(common.trim());
        let main_top = common.canonicalize().ok()?.parent()?.to_path_buf();
        (git_dir, main_top)
    };
    let main_top = main_top.canonicalize().ok()?;
    if main_top.parent()? != root.canonicalize().ok()? {
        return None;
    }
    let head = std::fs::read_to_string(git_dir.join("HEAD")).unwrap_or_default();
    Some(Here {
        repository: main_top.file_name()?.to_string_lossy().into_owned(),
        top: top.to_path_buf(),
        branch: head
            .trim()
            .strip_prefix("ref: refs/heads/")
            .map(str::to_string),
    })
}

/// The handover left in a working copy, as `toimija handover` wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    /// Unix seconds it was written.
    pub written_at: u64,
    pub unfinished: Option<String>,
    pub next: Option<String>,
}

/// A working copy of a repository: its own folder, or a linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingCopy {
    pub dir: PathBuf,
    /// `None` when HEAD is not on a branch.
    pub branch: Option<String>,
    pub handover: Option<Left>,
}

fn branch_at(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

fn handover_in(dir: &Path) -> Option<Left> {
    let text = std::fs::read_to_string(dir.join(".toimija/handover.json")).ok()?;
    let left: serde_json::Value = serde_json::from_str(&text).ok()?;
    let said = |key: &str| {
        left.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    Some(Left {
        written_at: left.get("written_at")?.as_u64()?,
        unfinished: said("unfinished"),
        next: said("next"),
    })
}

/// The working copies of the repository at `repo`: its own folder first,
/// then each linked worktree whose folder is still there. Read from the
/// files git keeps; no git is run.
pub fn working_copies(repo: &Path) -> Vec<WorkingCopy> {
    let git_dir = repo.join(".git");
    let mut copies = vec![WorkingCopy {
        dir: repo.to_path_buf(),
        branch: branch_at(&git_dir),
        handover: handover_in(repo),
    }];
    let Ok(linked) = std::fs::read_dir(git_dir.join("worktrees")) else {
        return copies;
    };
    let mut rest = Vec::new();
    for entry in linked.flatten() {
        // `gitdir` names the `.git` file inside the worktree's folder.
        let Ok(named) = std::fs::read_to_string(entry.path().join("gitdir")) else {
            continue;
        };
        let Some(dir) = Path::new(named.trim()).parent().map(Path::to_path_buf) else {
            continue;
        };
        if !dir.is_dir() {
            continue;
        }
        rest.push(WorkingCopy {
            branch: branch_at(&entry.path()),
            handover: handover_in(&dir),
            dir,
        });
    }
    rest.sort_by(|a, b| a.dir.cmp(&b.dir));
    copies.extend(rest);
    copies
}

/// The working copy a directory is inside, the deepest where they nest.
pub fn copy_of<'a>(copies: &'a [WorkingCopy], dir: &Path) -> Option<&'a WorkingCopy> {
    copies
        .iter()
        .filter(|copy| dir.starts_with(&copy.dir))
        .max_by_key(|copy| copy.dir.components().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_kernel_says_where_a_process_works() {
        let mine = cwd_of(std::process::id()).expect("this process has a directory");
        assert_eq!(
            mine.canonicalize().expect("it exists"),
            std::env::current_dir()
                .expect("the test has a directory")
                .canonicalize()
                .expect("it exists")
        );
        // A process that is not there has no directory: unknown, not a guess.
        assert_eq!(cwd_of(u32::MAX - 7), None);
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the folder is made");
        std::fs::write(path, text).expect("the file is written");
    }

    #[test]
    fn a_directory_belongs_to_the_repository_above_it_or_the_one_it_is_a_worktree_of() {
        let temp = tempfile::tempdir().expect("a temporary folder");
        let root = temp.path().join("work");
        write(&root.join("alpha/.git/HEAD"), "ref: refs/heads/main\n");
        std::fs::create_dir_all(root.join("alpha/src/deep")).expect("folders");
        assert_eq!(
            here(&root, &root.join("alpha/src/deep")),
            Some(Here {
                repository: "alpha".to_string(),
                top: root.join("alpha"),
                branch: Some("main".to_string()),
            })
        );

        // A linked worktree elsewhere in the workspace is its repository's.
        let linked = root.join("_worktrees/alpha-fix");
        let its_git = root.join("alpha/.git/worktrees/alpha-fix");
        write(&its_git.join("HEAD"), "ref: refs/heads/fix/one\n");
        write(&its_git.join("commondir"), "../..\n");
        write(
            &linked.join(".git"),
            &format!("gitdir: {}\n", its_git.display()),
        );
        std::fs::create_dir_all(linked.join("src")).expect("folders");
        assert_eq!(
            here(&root, &linked.join("src")),
            Some(Here {
                repository: "alpha".to_string(),
                top: linked.clone(),
                branch: Some("fix/one".to_string()),
            })
        );

        // A commit checked out by itself is on no branch.
        write(&root.join("beta/.git/HEAD"), "0123abcd\n");
        assert_eq!(
            here(&root, &root.join("beta")).and_then(|here| here.branch),
            None
        );
        // The repository's working copies: its own folder, then the worktree,
        // each with its branch and the handover left in it.
        write(
            &linked.join(".toimija/handover.json"),
            r#"{"written_at": 100, "unfinished": "the last step", "next": "  "}"#,
        );
        write(
            &its_git.join("gitdir"),
            &format!("{}\n", linked.join(".git").display()),
        );
        // A worktree whose folder was removed is not a place to work.
        write(
            &root.join("alpha/.git/worktrees/gone/gitdir"),
            &format!("{}\n", root.join("_worktrees/gone/.git").display()),
        );
        let copies = working_copies(&root.join("alpha"));
        assert_eq!(copies.len(), 2, "{copies:?}");
        assert_eq!(copies[0].branch.as_deref(), Some("main"));
        assert_eq!(copies[0].handover, None);
        assert_eq!(copies[1].branch.as_deref(), Some("fix/one"));
        assert_eq!(
            copies[1].handover,
            Some(Left {
                written_at: 100,
                unfinished: Some("the last step".to_string()),
                next: None,
            })
        );
        assert_eq!(
            copy_of(&copies, &linked.join("src")).and_then(|copy| copy.branch.as_deref()),
            Some("fix/one")
        );
        assert_eq!(copy_of(&copies, temp.path()), None);

        // The workspace folder itself, and a repository outside it, are no
        // repository of the workspace.
        assert_eq!(here(&root, &root), None);
        write(
            &temp.path().join("other/gamma/.git/HEAD"),
            "ref: refs/heads/main\n",
        );
        assert_eq!(here(&root, &temp.path().join("other/gamma")), None);
    }
}
