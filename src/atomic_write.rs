use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Stable marker shared with footprint classification for orphaned atomic
/// writes. The full suffix is `.tmp-<pid>-<sequence>`.
pub(crate) const TEMP_MARKER: &str = ".tmp-";

/// Atomically replace `path` with `contents`. The bytes are written and synced
/// in a temp file inside `staging`, then renamed onto `path`, so a reader sees
/// the old file or the new one, never a partial one. `staging` must be on the
/// same filesystem as `path` for the rename to be atomic. `before_rename` runs
/// on the synced temp file just before the rename publishes it.
pub(crate) fn replace_before_rename(
    path: &Path,
    staging: &Path,
    contents: &[u8],
    before_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let (temp, mut file) = loop {
        let temp = temp_path(path, staging);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        file.write_all(contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.sync_all()?;
        drop(file);
        before_rename(&temp)?;
        fs::rename(&temp, path)?;
        #[cfg(unix)]
        OpenOptions::new().read(true).open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// `<staging>/<target file name>.tmp-<pid>-<sequence>`: the target's name
/// stays legible in the temp's, so an orphan says what it was for.
fn temp_path(path: &Path, staging: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| OsString::from("artifact"), OsString::from);
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    name.push(format!("{TEMP_MARKER}{}-{sequence}", std::process::id()));
    staging.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_before_rename_preserves_the_old_file() {
        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join("staging");
        fs::create_dir(&staging).unwrap();
        let path = temp.path().join("current.json");
        fs::write(&path, b"old complete json").unwrap();

        let error = replace_before_rename(&path, &staging, b"new complete json", |_| {
            Err(io::Error::other("injected before rename"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&path).unwrap(), b"old complete json");
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    }

    #[test]
    fn the_temp_file_is_staged_apart_from_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join("staging");
        fs::create_dir(&staging).unwrap();
        let path = temp.path().join("current.json");

        replace_before_rename(&path, &staging, b"{}", |temp_file| {
            assert_eq!(temp_file.parent(), Some(staging.as_path()));
            assert!(
                temp_file
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("current.json.tmp-")
            );
            Ok(())
        })
        .unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    #[test]
    fn concurrent_readers_only_observe_complete_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.json");
        let replace = |bytes: &[u8]| replace_before_rename(&path, temp.path(), bytes, |_| Ok(()));
        replace(br#"{"value":0}"#).unwrap();

        std::thread::scope(|scope| {
            scope.spawn(|| {
                for value in 1..100 {
                    replace(format!(r#"{{"value":{value}}}"#).as_bytes()).unwrap();
                }
            });
            for _ in 0..500 {
                let bytes = fs::read(&path).unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
            }
        });
    }
}
