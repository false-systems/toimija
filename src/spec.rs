//! A spec a session starts from: a file in the repository (an ADR, say) or an
//! issue reference.
//!
//! A spec is a fact the session declares, like its intent. Toimija records
//! the reference in the session marker and shows it. It reads a file spec for
//! its title and stated status; it never fetches an issue and never checks
//! the work against either.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

/// Where `spec new` and `spec list` look when `.toimija/repo.yaml` names no
/// `specs:` folder.
pub const DEFAULT_FOLDER: &str = "docs/adr";

/// The reason a file spec carries in Read first.
pub const READ_FIRST_REASON: &str = "session spec";

/// Whether `reference` is an issue reference: `#` and digits, or a URL.
/// Anything else is a path.
pub fn is_issue(reference: &str) -> bool {
    let number = reference
        .strip_prefix('#')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
    number || reference.contains("://")
}

/// The reference to store for `given`. An issue reference is kept exactly as
/// given. A path is read relative to `cwd`, must name an existing file inside
/// `root`, and is stored relative to `root`. Both sides are canonicalized
/// before the inside check.
pub fn resolve(root: &Path, cwd: &Path, given: &str) -> Result<String> {
    if is_issue(given) {
        return Ok(given.to_string());
    }
    let path = cwd.join(given);
    let Ok(file) = path.canonicalize() else {
        bail!("spec file {given} does not exist");
    };
    if !file.is_file() {
        bail!("spec {given} is not a file");
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", root.display()))?;
    let Ok(relative) = file.strip_prefix(&root) else {
        bail!(
            "spec file {given} is outside the repository ({}): a spec is a file in the \
             repository, `#<number>` or a URL",
            root.display()
        );
    };
    // Stored as the path itself, never an escaped form of it: the reference is
    // read back as a path. A marker holds text, and the reference is printed
    // on one line, so a path that is not UTF-8 or holds a control character
    // is refused.
    let Some(text) = relative.to_str() else {
        bail!("spec file {given} has a name that is not valid UTF-8");
    };
    if text.chars().any(char::is_control) {
        bail!("spec file {given:?} has a control character in its name");
    }
    Ok(text.to_string())
}

/// What a packet says about a session's spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecBlock {
    /// The path relative to the repository root, or the issue reference as
    /// given.
    pub reference: String,
    pub kind: SpecKind,
    /// The file's first `# ` heading. Absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the file states on a line beginning `Status:`. Absent when it
    /// states none: never guessed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Why the file could not be read, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecKind {
    File,
    Issue,
}

/// Describe the spec `reference` of a session in `root`. A file is read for
/// its title and status; an issue reference is only repeated.
pub fn describe(root: &Path, reference: &str) -> SpecBlock {
    if is_issue(reference) {
        return SpecBlock {
            reference: reference.to_string(),
            kind: SpecKind::Issue,
            title: None,
            status: None,
            unreadable: None,
        };
    }
    let read = resolved_inside(root, Path::new(reference))
        .map_err(|error| format!("{error:#}"))
        .and_then(|_| fs::read_to_string(root.join(reference)).map_err(|error| error.to_string()));
    let (facts, unreadable) = match read {
        Ok(text) => (facts(&text), None),
        Err(reason) => (Facts::default(), Some(reason)),
    };
    SpecBlock {
        reference: reference.to_string(),
        kind: SpecKind::File,
        title: facts.title,
        status: facts.status,
        unreadable,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Facts {
    title: Option<String>,
    status: Option<String>,
}

/// The first `# ` heading and the first line beginning `Status:`.
fn facts(text: &str) -> Facts {
    let stated = |prefix: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Facts {
        title: stated("# "),
        status: stated("Status:"),
    }
}

/// `root.join(relative)` with every symlink resolved, refused when that is
/// outside `root`. Where the path does not exist yet, its deepest existing
/// folder is what gets resolved, so a symlink above a folder that is still to
/// be created is caught before anything is created through it.
fn resolved_inside(root: &Path, relative: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", root.display()))?;
    let wanted = root.join(relative);
    let existing = wanted
        .ancestors()
        .find(|path| fs::symlink_metadata(path).is_ok())
        .unwrap_or(&root);
    let resolved = existing
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", existing.display()))?;
    if !resolved.starts_with(&root) {
        bail!(
            "{} resolves outside the repository ({}): a spec stays inside it",
            relative.display(),
            resolved.display()
        );
    }
    Ok(resolved)
}

/// The folder specs live in, relative to the repository root. It must stay
/// inside the repository.
pub fn folder(configured: Option<&Path>) -> Result<PathBuf> {
    let folder = configured.unwrap_or(Path::new(DEFAULT_FOLDER));
    let inside = folder
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    if !inside || folder.as_os_str().is_empty() {
        bail!(
            "`specs:` in .toimija/repo.yaml must be a folder inside the repository, not {}",
            folder.display()
        );
    }
    Ok(folder.to_path_buf())
}

/// A spec file in the folder: `<four digits>-<name>.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Relative to the repository root.
    pub path: PathBuf,
    pub number: u32,
    pub title: Option<String>,
    pub status: Option<String>,
}

fn number_of(file_name: &str) -> Option<u32> {
    let (digits, rest) = file_name.split_at_checked(4)?;
    (digits.bytes().all(|b| b.is_ascii_digit()) && rest.starts_with('-') && rest.ends_with(".md"))
        .then(|| digits.parse().ok())
        .flatten()
}

/// The numbered spec files in `folder` (relative to `root`), by number then
/// name. A folder that does not exist holds none.
pub fn list(root: &Path, folder: &Path) -> Result<Vec<Listed>> {
    let dir = root.join(folder);
    resolved_inside(root, folder)?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    let mut specs = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(number) = number_of(&name) else {
            continue;
        };
        // A numbered name that links out of the repository is refused, not
        // read: its title and status would come from outside.
        resolved_inside(root, &folder.join(&name))?;
        if !entry.path().is_file() {
            continue;
        }
        let facts = fs::read_to_string(entry.path())
            .map(|text| facts(&text))
            .with_context(|| format!("failed to read {}", entry.path().display()))?;
        specs.push(Listed {
            path: folder.join(&name),
            number,
            title: facts.title,
            status: facts.status,
        });
    }
    specs.sort_by(|a, b| a.number.cmp(&b.number).then_with(|| a.path.cmp(&b.path)));
    Ok(specs)
}

/// The file name part made of `title`: lowercase letters and digits, with
/// every run of anything else as one hyphen.
fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// What a new spec holds.
fn template(title: &str) -> String {
    format!("# {title}\n\nStatus: proposed\n\n## Context\n\n## Decision\n\n## Consequences\n")
}

/// Create the next numbered spec for `title` in `folder` and return its path
/// relative to `root`. The number is one more than the highest there. It
/// writes that one file and never replaces an existing one.
pub fn create(root: &Path, folder: &Path, title: &str) -> Result<PathBuf> {
    use std::io::Write as _;
    let title = title.trim();
    let slug = slug(title);
    if slug.is_empty() {
        bail!("a spec title needs at least one letter or digit to name its file");
    }
    let next = list(root, folder)?
        .iter()
        .map(|spec| spec.number)
        .max()
        .unwrap_or(0)
        + 1;
    if next > 9999 {
        bail!("{} already holds spec 9999", folder.display());
    }
    let relative = folder.join(format!("{next:04}-{slug}.md"));
    let path = root.join(&relative);
    let dir = root.join(folder);
    // Checked before the folder is created and again after, so no symlink in
    // or above it takes the new file out of the repository.
    resolved_inside(root, folder)?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    resolved_inside(root, folder)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("refusing to overwrite {}", relative.display()))?;
    file.write_all(template(title).as_bytes())
        .with_context(|| format!("failed to write {}", relative.display()))?;
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issue_reference_is_a_number_or_a_url_and_nothing_else() {
        assert!(is_issue("#123"));
        assert!(is_issue("https://tracker.example.invalid/issues/7"));
        assert!(!is_issue("#"));
        assert!(!is_issue("#12a"));
        assert!(!is_issue("docs/adr/0001-login.md"));
        assert!(!is_issue("123"));
    }

    #[test]
    fn title_and_status_are_read_only_where_the_file_states_them() {
        assert_eq!(
            facts("intro\n# Login redirect\n\nStatus: accepted \n# Second\nStatus: later\n"),
            Facts {
                title: Some("Login redirect".to_string()),
                status: Some("accepted".to_string()),
            }
        );
        assert_eq!(
            facts("## Status\n\naccepted\nstatus: no\n#NoSpace\n"),
            Facts::default()
        );
        assert_eq!(facts("Status:\n# \n"), Facts::default());
    }

    #[test]
    fn a_slug_is_lowercase_with_single_hyphens() {
        assert_eq!(
            slug("Rate limit the Login API!"),
            "rate-limit-the-login-api"
        );
        assert_eq!(slug("  --A  b__c--  "), "a-b-c");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn only_four_digit_markdown_files_are_numbered_specs() {
        assert_eq!(number_of("0007-login.md"), Some(7));
        assert_eq!(number_of("0007-login.txt"), None);
        assert_eq!(number_of("007-login.md"), None);
        assert_eq!(number_of("00071-login.md"), None);
        assert_eq!(number_of("README.md"), None);
    }

    #[test]
    fn create_never_replaces_a_file_and_the_folder_stays_inside() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let folder = Path::new("docs/adr");
        assert_eq!(
            create(root, folder, "First").unwrap(),
            Path::new("docs/adr/0001-first.md")
        );
        // A name the numbering cannot see (a directory) is in the way of the
        // next file: refused, not replaced.
        fs::create_dir(root.join("docs/adr/0002-second.md")).unwrap();
        let error = format!("{:#}", create(root, folder, "Second").unwrap_err());
        assert!(error.contains("refusing to overwrite"), "{error}");

        assert!(super::folder(Some(Path::new("../elsewhere"))).is_err());
        assert!(super::folder(Some(Path::new("/abs"))).is_err());
        assert_eq!(super::folder(None).unwrap(), Path::new(DEFAULT_FOLDER));
    }

    /// A symlink must not carry a spec out of the repository: not the spec
    /// folder itself, not a folder above it, and not a file given as a spec.
    #[cfg(unix)]
    #[test]
    fn a_symlink_never_takes_a_spec_outside_the_repository() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(outside.join("adr")).unwrap();
        fs::write(
            outside.join("adr/0003-theirs.md"),
            "# Theirs\n\nStatus: secret\n",
        )
        .unwrap();
        let listing = || {
            let mut names: Vec<_> = fs::read_dir(outside.join("adr"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = listing();

        // The spec folder itself is a symlink out.
        fs::create_dir_all(root.join("docs")).unwrap();
        symlink(outside.join("adr"), root.join("docs/adr")).unwrap();
        let error = format!(
            "{:#}",
            create(&root, Path::new("docs/adr"), "Mine").unwrap_err()
        );
        assert!(error.contains("outside the repository"), "{error}");
        let error = format!("{:#}", list(&root, Path::new("docs/adr")).unwrap_err());
        assert!(error.contains("outside the repository"), "{error}");

        // A folder above it is a symlink out, and the spec folder is not there yet.
        symlink(&outside, root.join("linked")).unwrap();
        let error = format!(
            "{:#}",
            create(&root, Path::new("linked/new"), "Mine").unwrap_err()
        );
        assert!(error.contains("outside the repository"), "{error}");
        assert!(
            !outside.join("new").exists(),
            "a folder was created outside"
        );
        assert_eq!(listing(), before, "a file was written outside");

        // A file given as a spec that resolves outside.
        symlink(
            outside.join("adr/0003-theirs.md"),
            root.join("docs/link.md"),
        )
        .unwrap();
        let error = resolve(&root, &root, "docs/link.md").unwrap_err();
        assert!(
            error.to_string().contains("outside the repository"),
            "{error}"
        );
        // A numbered file in the folder that is a link out is not listed: its
        // title and status would be read from outside.
        fs::remove_file(root.join("docs/adr")).unwrap();
        fs::create_dir(root.join("docs/adr")).unwrap();
        fs::write(root.join("docs/adr/0001-mine.md"), "# Mine\n").unwrap();
        symlink(
            outside.join("adr/0003-theirs.md"),
            root.join("docs/adr/0002-link.md"),
        )
        .unwrap();
        let error = format!("{:#}", list(&root, Path::new("docs/adr")).unwrap_err());
        assert!(error.contains("0002-link.md"), "{error}");
        assert!(error.contains("outside the repository"), "{error}");
        assert!(!error.contains("secret"), "{error}");
        // And one that becomes such a link after it was declared is not read.
        let block = describe(&root, "docs/link.md");
        assert_eq!(block.title, None);
        assert_eq!(block.status, None);
        assert!(block.unreadable.unwrap().contains("outside the repository"));
    }

    /// The stored reference is the path itself, so what is read later is the
    /// file that was given: nothing is escaped on the way in.
    #[cfg(unix)]
    #[test]
    fn a_spec_path_is_stored_as_it_is_and_reads_back_the_same_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("docs")).unwrap();
        let name = r"docs/back\slash spec.md";
        fs::write(root.join(name), "# Backslash\n\nStatus: accepted\n").unwrap();

        let stored = resolve(&root, &root, name).unwrap();
        assert_eq!(stored, name);
        let block = describe(&root, &stored);
        assert_eq!(block.unreadable, None);
        assert_eq!(block.title.as_deref(), Some("Backslash"));

        // A name that would break a line of output is refused, not escaped
        // into another name.
        fs::write(root.join("docs/two\nlines.md"), "# Two\n").unwrap();
        let error = resolve(&root, &root, "docs/two\nlines.md").unwrap_err();
        assert!(error.to_string().contains("control character"), "{error}");
    }

    #[test]
    fn a_path_is_stored_relative_to_the_root_and_must_be_inside_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("docs/spec.md"), "# Spec\n").unwrap();
        fs::write(temp.path().join("outside.md"), "# Outside\n").unwrap();

        assert_eq!(
            resolve(&root, &root.join("src"), "../docs/spec.md").unwrap(),
            "docs/spec.md"
        );
        assert_eq!(resolve(&root, &root, "#12").unwrap(), "#12");
        let missing = resolve(&root, &root, "docs/none.md").unwrap_err();
        assert!(missing.to_string().contains("docs/none.md"));
        let outside = resolve(&root, &root, "../outside.md").unwrap_err();
        assert!(outside.to_string().contains("outside the repository"));
        assert!(resolve(&root, &root, "docs").is_err());
    }
}
