//! The faija-terms crate Toimija carries is the one faija wrote.
//!
//! faija owns tree-identity.v1, the canonical identity of a working State.
//! Toimija holds that crate under `vendor/` rather than fetching it from a
//! private repository on every build, so there is still exactly one
//! implementation — distributed rather than linked.
//!
//! Like `tests/vendored_canonical.rs`, this is deliberately **not** a security
//! boundary: the same commit could change the vendored crate and the digest
//! recorded beside it. What it catches is drift: an edit here fails the build,
//! and what a reviewer sees is a diff touching `vendor/`.

use std::path::{Path, PathBuf};

use faija_terms::TreeIdentityAlgorithm;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The construction `scripts/vendor-faija-terms.sh` records: one line per
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
        .unwrap_or_else(|| panic!("faija-terms.contract has no {key}"))
}

fn contract() -> String {
    std::fs::read_to_string(repo_root().join("vendor/faija-terms.contract"))
        .expect("vendor/faija-terms.contract")
}

#[test]
fn the_vendored_crate_is_the_one_the_contract_file_names() {
    let recorded = contract();
    let (digest, files) = tree_digest(&repo_root().join("vendor/faija-terms"));

    assert_eq!(
        digest,
        field(&recorded, "tree-digest"),
        "the vendored crate differs from the revision the contract file names; \
         refresh it with scripts/vendor-faija-terms.sh, never by editing it here"
    );
    assert_eq!(files.to_string(), field(&recorded, "files"));
}

#[test]
fn the_contract_file_names_a_revision_and_says_whether_it_is_tagged() {
    let recorded = contract();
    let revision = field(&recorded, "faija-revision");
    assert_eq!(revision.len(), 40, "a full commit id, not a short one");
    assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
    let tag = field(&recorded, "faija-tag");
    assert!(
        tag == "untagged" || tag.starts_with('v'),
        "the tag is a release tag or says plainly that there is none: {tag}"
    );
}

/// What Toimija executes is the frozen tree-identity.v1, not whatever a later
/// re-pin happens to call v1. faija's own test pins the same digest; this one
/// fails *here* if a re-vendor ever carried a different definition.
#[test]
fn the_vendored_tree_identity_is_the_frozen_v1() {
    let v1 = TreeIdentityAlgorithm::V1;
    assert_eq!(v1.id(), "tree-identity.v1");
    assert_eq!(v1.namespaces(), &[".toimija", ".teko", ".sykli"]);
    assert_eq!(v1.config_paths(), &[".toimija/repo.yaml"]);
    assert_eq!(v1.definition_digest(), "fnv1a64:bf0d7d0913aa2b1f");
}
