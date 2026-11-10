//! The canonical crate Toimija carries is byte-identical to its source.
//!
//! That crate owns the envelope, its canonical byte string and every digest over it.
//! Toimija holds that implementation under `vendor/` rather than fetching it
//! from a private repository on every build, so there is still exactly one
//! implementation — distributed rather than linked.
//!
//! This test is deliberately **not** a security boundary. The same commit could
//! change the vendored crate and the digest recorded beside it. What it catches
//! is drift: an edit here fails the build, and what a reviewer sees is a diff
//! touching `vendor/`, which is the point.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The construction `scripts/vendor-sauma-canonical.sh` records: one line per
/// file, `<relative path>  <sha256>`, sorted by path, digested whole. A rename,
/// a reorder or a truncation all change it.
fn tree_digest(dir: &Path) -> (String, usize) {
    use sha2::{Digest, Sha256};

    fn walk(dir: &Path, prefix: &Path, out: &mut Vec<(String, PathBuf)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = prefix.join(entry.file_name());
            if path.is_dir() {
                walk(&path, &relative, out);
            } else {
                out.push((relative.to_string_lossy().into_owned(), path));
            }
        }
    }

    let mut files = Vec::new();
    walk(dir, Path::new(""), &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut listing = String::new();
    for (relative, path) in &files {
        let bytes = std::fs::read(path).expect("read vendored file");
        listing.push_str(&format!("{relative}  {:x}\n", Sha256::digest(&bytes)));
    }
    // The script digests the listing without its final newline (`printf '%s'`).
    let trimmed = listing.strip_suffix('\n').unwrap_or(&listing);
    (
        format!("sha256:{:x}", Sha256::digest(trimmed.as_bytes())),
        files.len(),
    )
}

fn field<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim())
        .unwrap_or_else(|| panic!("sauma-canonical.contract has no {key}"))
}

#[test]
fn the_vendored_crate_is_the_one_the_contract_file_names() {
    let root = repo_root();
    let recorded = std::fs::read_to_string(root.join("vendor/sauma-canonical.contract"))
        .expect("vendor/sauma-canonical.contract");
    let (digest, files) = tree_digest(&root.join("vendor/sauma-canonical"));

    assert_eq!(
        digest,
        field(&recorded, "tree-digest"),
        "the vendored crate differs from the revision the contract file names; \
         refresh it with scripts/vendor-sauma-canonical.sh, never by editing it here"
    );
    assert_eq!(files.to_string(), field(&recorded, "files"));
}

#[test]
fn the_contract_file_names_a_revision() {
    let recorded = std::fs::read_to_string(repo_root().join("vendor/sauma-canonical.contract"))
        .expect("vendor/sauma-canonical.contract");
    let revision = field(&recorded, "sauma-revision");
    assert_eq!(revision.len(), 40, "a full commit id, not a short one");
    assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
}
