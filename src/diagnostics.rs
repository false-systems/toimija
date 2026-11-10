//! Descriptor-level terminal isolation for the wrapped window.
//!
//! `toimija run` wraps a harness that owns the terminal. While the wrapped
//! command is alive, nothing from this process may reach its terminal — not
//! watcher diagnostics, not refresh warnings, not panic messages, not output
//! from some future code path that was never reviewed for this rule.
//!
//! The guarantee is therefore not a convention but file-descriptor topology:
//! [`Capture::borrow`] re-points this process's own stdout and stderr at the
//! session `diagnostics.log` with `dup2`, keeping dups of the real terminal to
//! hand to the wrapped child. From then on the process *cannot* write to the
//! harness's terminal — it holds no descriptor that points at it — and every
//! stray line lands in the log instead. [`Capture::finish`] restores the
//! terminal (so the exit summary prints normally) and reports how many
//! diagnostic lines were captured.
//!
//! This is foreground-only state for the wrapped window of one command; it
//! never outlives the guard and involves no daemon or network.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

/// How many non-empty lines the capture log holds.
fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|contents| {
            contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::Stdio;

    /// The borrowed terminal: this process's stdout/stderr point at the log,
    /// while `saved_*` keep the real terminal for the wrapped child and for
    /// restoration.
    pub struct Capture {
        saved_out: OwnedFd,
        saved_err: OwnedFd,
        path: PathBuf,
    }

    fn dup_fd(fd: i32) -> io::Result<OwnedFd> {
        // SAFETY: `dup` duplicates the descriptor; the returned fd is new and
        // exclusively ours, so wrapping it in `OwnedFd` is sound.
        let copy = unsafe { libc::dup(fd) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `copy` is a fresh fd we own (see above).
        Ok(unsafe { OwnedFd::from_raw_fd(copy) })
    }

    fn redirect(src: i32, dst: i32) -> io::Result<()> {
        // SAFETY: plain `dup2` of live descriptors; on failure `dst` may be
        // closed, which the caller treats as a failed capture.
        if unsafe { libc::dup2(src, dst) } < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    impl Capture {
        /// Truncate (a fresh session makes it empty anyway) and re-point fd 1
        /// and fd 2 of this process at the log. Callers print everything they
        /// want on the real terminal *before* this.
        pub fn borrow(log_path: &Path) -> io::Result<Self> {
            // Never through a symlink planted at the log's path.
            let log = crate::owned::open_nofollow(
                log_path,
                OpenOptions::new().create(true).append(true),
            )?;
            // A fresh session makes the log empty anyway; truncate explicitly
            // so a reused path cannot inflate the capture count.
            log.set_len(0)?;
            let saved_out = dup_fd(1)?;
            let saved_err = dup_fd(2)?;
            // Anything still buffered for the old stdout must not silently
            // become the first log line.
            let _ = std::io::stdout().flush();
            redirect(log.as_raw_fd(), 1)?;
            redirect(log.as_raw_fd(), 2)?;
            // fd 1/2 now hold their own references to the log's open file
            // description; this handle can close without disturbing them.
            drop(log);
            Ok(Capture {
                saved_out,
                saved_err,
                path: log_path.to_path_buf(),
            })
        }

        /// The real terminal, for the wrapped child's stdout. While captured,
        /// the child's default-inherit would otherwise point at our log.
        pub fn child_stdout(&self) -> io::Result<Stdio> {
            Ok(Stdio::from(self.saved_out.try_clone()?))
        }

        /// The real terminal, for the wrapped child's stderr.
        pub fn child_stderr(&self) -> io::Result<Stdio> {
            Ok(Stdio::from(self.saved_err.try_clone()?))
        }

        fn restore(&mut self) {
            let _ = std::io::stdout().flush();
            let _ = redirect(self.saved_out.as_raw_fd(), 1);
            let _ = redirect(self.saved_err.as_raw_fd(), 2);
        }

        /// Give the terminal back and count what was captured. The wrapped
        /// command and the watcher must both be done by now, or their last
        /// lines would race the restore.
        pub fn finish(mut self) -> usize {
            self.restore();
            count_lines(&self.path)
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            self.restore();
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::path::Path;
    use std::process::Stdio;

    /// Without descriptor duplication there is no isolation to perform: the
    /// wrapped command shares our terminal.
    /// The platforms this crate targets (see signal forwarding) are Unix.
    pub struct Capture;

    impl Capture {
        pub fn borrow(_log_path: &Path) -> io::Result<Self> {
            Ok(Capture)
        }

        pub fn child_stdout(&self) -> io::Result<Stdio> {
            Ok(Stdio::inherit())
        }

        pub fn child_stderr(&self) -> io::Result<Stdio> {
            Ok(Stdio::inherit())
        }

        pub fn finish(self) -> usize {
            0
        }
    }
}

pub use imp::Capture;

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// fd redirection is process-global; serialize tests that flip it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn captured_writes_land_in_the_log_and_restoration_is_clean() {
        let _test = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("diagnostics.log");

        let capture = Capture::borrow(&log).unwrap();
        // The libtest harness intercepts print!/eprintln! through a
        // thread-local buffer before fd 2 is ever reached, so prove the
        // descriptor topology with the channels that exist in production: a
        // raw fd-2 write and a child that inherits our stderr.
        // SAFETY: writing a constant string to a live descriptor.
        unsafe {
            let message = b"captured raw fd write\n";
            libc::write(2, message.as_ptr().cast(), message.len());
        }
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo captured inherited child >&2")
            .status()
            .unwrap();
        assert!(status.success());

        let count = capture.finish();
        assert!(
            count >= 2,
            "both captured writes must be counted, got {count}"
        );
        let contents = std::fs::read_to_string(&log).unwrap();
        assert!(contents.contains("captured raw fd write"), "{contents}");
        assert!(contents.contains("captured inherited child"), "{contents}");

        // Restored: a later fd-2 write must not append to the log (it goes to
        // the real stderr, i.e. test output).
        // SAFETY: writing a constant string to a live descriptor.
        unsafe {
            let message = b"after restore - not for the log\n";
            libc::write(2, message.as_ptr().cast(), message.len());
        }
        let contents = std::fs::read_to_string(&log).unwrap();
        assert!(!contents.contains("after restore"), "{contents}");
    }

    #[test]
    fn the_wrapped_child_writes_to_the_real_terminal_not_the_log() {
        let _test = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("diagnostics.log");

        let capture = Capture::borrow(&log).unwrap();
        // The child gets the saved terminal: its noise must never reach the
        // capture log. (`status()` honors the explicit stderr override; the
        // echo lands in this test binary's own stderr, which is the point.)
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo child-noise >&2")
            .stderr(capture.child_stderr().unwrap())
            .status()
            .unwrap();
        assert!(status.success());

        let _count = capture.finish();
        let contents = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!contents.contains("child-noise"), "{contents}");
    }

    #[test]
    fn each_capture_truncates_so_the_count_is_its_own() {
        let _test = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("diagnostics.log");
        std::fs::write(&log, "left over from a previous run\n").unwrap();

        let capture = Capture::borrow(&log).unwrap();
        let _count = capture.finish();
        let contents = std::fs::read_to_string(&log).unwrap();
        assert!(!contents.contains("left over"), "{contents}");
    }
}
