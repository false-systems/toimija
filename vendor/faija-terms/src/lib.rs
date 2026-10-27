//! `tree-identity.v1`: which content makes up one version of a repository's
//! work.
//!
//! The identity is the Git tree of every tracked and untracked non-ignored
//! path, minus the namespaces of generated tool state frozen into the version,
//! plus the tool configuration it forces in. [`TreeIdentityAlgorithm::plan`]
//! returns the procedure as data, and each tool runs the git steps with its own
//! runner. This crate never executes anything, reads a repository, stores
//! state, or decides whether a claim is true.
//!
//! # Where the meaning comes from
//!
//! Tree identity is defined in `faija-ontology.v1` (`docs/ontology-v1.md`).
//! `faija-ontology.v2` replaced v1 as the ontology and keeps `tree-identity.v1`
//! as a contract; it is the only part of v1 still in use. An artifact records
//! the algorithm's [`id`](TreeIdentityAlgorithm::id), so a reader years later
//! recomputes the identity under the rules that applied when it was written.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// A versioned procedure for computing repository tree identity.
///
/// The algorithm is part of the contract. Its inputs — which namespaces are
/// excluded, which configuration is forced in, the order of the git steps — are
/// frozen per version. Changing any of them changes what historical artifacts
/// would mean, so it requires a new variant rather than an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TreeIdentityAlgorithm {
    /// `tree-identity.v1`. See [`TreeIdentityAlgorithm::definition`].
    V1,
}

/// Namespaces excluded by `tree-identity.v1`. **Frozen.**
///
/// A namespace is here because its owner *demonstrably* generates
/// repository-local derived state whose mutation must not change what work is
/// being identified. It is not here because the owner is a False Systems tool.
/// Under this algorithm version, the contents of these directories are defined
/// as generated operational state — that is a statement about repository
/// semantics, not a registry of products.
///
/// Speculative reservation is forbidden. Excluding a namespace nobody writes to
/// would silently erase meaningful content a developer later puts there, which
/// is strictly worse than carrying no entry at all. `.kisko` and `.faija` were
/// considered and rejected on exactly this ground: neither exists anywhere in
/// the estate, Kisko's signer trust root lives in user configuration, and Faija
/// is constitutionally non-persistent. Kisko-related generated state that does
/// exist lives under `.toimija/`.
///
/// Before adding an entry, answer all four concretely:
///   1. Why is this directory excluded?
///   2. What generated state lives there today?
///   3. Who owns that state?
///   4. Why must changing it NOT change work identity?
///
/// If any answer is speculative, the namespace does not belong.
///
/// **The set never grows.** There is no dynamic registration: reading namespaces
/// from configuration would let today's setup reinterpret yesterday's
/// identities. A future tool needing exclusion is a `V2` decision, and that
/// being mildly annoying is the point — changing what counts as work reality
/// should be expensive and explicit.
const V1_NAMESPACES: &[&str] = &[".toimija", ".teko", ".sykli"];

/// Configuration paths force-included by `tree-identity.v1`. **Frozen**, for
/// the same reason as [`V1_NAMESPACES`].
const V1_CONFIG_PATHS: &[&str] = &[".toimija/repo.yaml"];

impl TreeIdentityAlgorithm {
    /// The current algorithm for new work. Historical artifacts name their own.
    pub const CURRENT: Self = Self::V1;

    /// Stable identifier recorded in artifacts.
    pub const fn id(self) -> &'static str {
        match self {
            Self::V1 => "tree-identity.v1",
        }
    }

    /// Parse an identifier from a historical artifact. Returns `None` for an
    /// algorithm this build does not know — which is a refusal to guess, not an
    /// error to paper over.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "tree-identity.v1" => Some(Self::V1),
            _ => None,
        }
    }

    /// Tool namespaces this algorithm excludes.
    pub const fn namespaces(self) -> &'static [&'static str] {
        match self {
            Self::V1 => V1_NAMESPACES,
        }
    }

    /// Tool configuration this algorithm force-includes.
    pub const fn config_paths(self) -> &'static [&'static str] {
        match self {
            Self::V1 => V1_CONFIG_PATHS,
        }
    }

    /// Prose definition, kept beside the code that implements it.
    pub const fn definition(self) -> &'static str {
        match self {
            Self::V1 => {
                "\
tree_identity is the Git tree OID of the repository's work content: every \
tracked and untracked non-ignored path, excluding every tool namespace frozen \
into this algorithm version, plus every declared tool-configuration path, \
force-included regardless of ignore status."
            }
        }
    }

    /// Git pathspecs excluding each namespace.
    ///
    /// `[.]` is a glob character class matching a literal dot. It stops Git from
    /// treating the namespace as an explicit path, which would otherwise
    /// override `.gitignore` for a directory meant to stay out of the tree.
    pub fn excludes(self) -> Vec<String> {
        self.namespaces()
            .iter()
            .map(|namespace| {
                let bare = namespace.strip_prefix('.').unwrap_or(namespace);
                format!(":(exclude,top,glob)[.]{bare}/**")
            })
            .collect()
    }

    /// The canonical procedure, in order, against a throwaway index.
    ///
    /// Returned as data rather than executed, so each tool runs it with its own
    /// process runner and error handling while performing the same git
    /// operations in the same order.
    pub fn plan(self) -> Vec<Step> {
        let mut plan = vec![
            Step::SeedFromHead,
            Step::OverlayWorktree {
                excludes: self.excludes(),
            },
            Step::DropNamespaces {
                paths: self.namespaces().iter().map(|p| p.to_string()).collect(),
            },
        ];
        plan.extend(self.config_paths().iter().map(|path| Step::ForceAddConfig {
            path: path.to_string(),
        }));
        plan.push(Step::WriteTree);
        plan
    }

    /// A digest over everything that determines the algorithm's behaviour.
    ///
    /// Pinned by test. An edit to a frozen list or to the step order changes
    /// this value and fails the build, which is the mechanism that makes
    /// "frozen" mean something rather than being a comment asking nicely.
    pub fn definition_digest(self) -> String {
        let mut material = String::new();
        material.push_str(self.id());
        material.push('\n');
        for namespace in self.namespaces() {
            material.push_str("namespace\t");
            material.push_str(namespace);
            material.push('\n');
        }
        for path in self.config_paths() {
            material.push_str("config\t");
            material.push_str(path);
            material.push('\n');
        }
        for step in self.plan() {
            material.push_str("step\t");
            material.push_str(step.name());
            material.push('\n');
        }
        format!("fnv1a64:{:016x}", fnv1a64(material.as_bytes()))
    }
}

/// A small, dependency-free digest. Sufficient for pinning a definition in a
/// test; deliberately *not* a digest for artifacts, which this crate never
/// computes.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One step of a tree-identity procedure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// `read-tree HEAD`. Failure is tolerated: an unborn HEAD has no tracked
    /// files. Seeding matters because into an empty index `add -A` treats every
    /// path as untracked and applies `.gitignore`, dropping committed-but-
    /// ignored content that HEAD legitimately contains.
    SeedFromHead,
    /// `add -A -- <excludes>`. Excludes are applied here, not only by the later
    /// removal, so Git never stats a file inside a namespace that an atomic
    /// write may be replacing underneath it.
    OverlayWorktree { excludes: Vec<String> },
    /// `rm --cached -r -q --ignore-unmatch <paths>`. Drops namespace content
    /// that `SeedFromHead` brought in because it was tracked.
    DropNamespaces { paths: Vec<String> },
    /// `add -f -- <path>`, only when the path exists in the worktree.
    ForceAddConfig { path: String },
    /// `write-tree`. Its output is the tree identity.
    WriteTree,
}

impl Step {
    /// Stable name, used for the definition digest.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::SeedFromHead => "seed_from_head",
            Self::OverlayWorktree { .. } => "overlay_worktree",
            Self::DropNamespaces { .. } => "drop_namespaces",
            Self::ForceAddConfig { .. } => "force_add_config",
            Self::WriteTree => "write_tree",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test that makes "frozen" enforceable. If you are here because this
    /// failed, you changed `tree-identity.v1`. Add `V2` instead: editing v1
    /// silently reinterprets every tree identity ever recorded under it.
    #[test]
    fn v1_definition_is_frozen() {
        let v1 = TreeIdentityAlgorithm::V1;
        assert_eq!(v1.namespaces(), &[".toimija", ".teko", ".sykli"]);
        assert_eq!(v1.config_paths(), &[".toimija/repo.yaml"]);
        assert_eq!(
            v1.plan().iter().map(Step::name).collect::<Vec<_>>(),
            vec![
                "seed_from_head",
                "overlay_worktree",
                "drop_namespaces",
                "force_add_config",
                "write_tree"
            ]
        );
        assert_eq!(v1.definition_digest(), "fnv1a64:bf0d7d0913aa2b1f");
    }

    /// The other half of the rule. A directory nobody reserved is ordinary
    /// work content, and must be treated as such — this is what speculative
    /// reservation would have silently broken.
    #[test]
    fn an_unreserved_namespace_is_ordinary_work_content() {
        let v1 = TreeIdentityAlgorithm::V1;
        for candidate in [".kisko", ".faija", ".mummo", ".github"] {
            assert!(
                !v1.namespaces().contains(&candidate),
                "{candidate} is reserved without demonstrated generated state"
            );
        }
    }

    #[test]
    fn an_unknown_algorithm_is_refused_rather_than_guessed() {
        assert_eq!(
            TreeIdentityAlgorithm::from_id("tree-identity.v1"),
            Some(TreeIdentityAlgorithm::V1)
        );
        assert_eq!(TreeIdentityAlgorithm::from_id("tree-identity.v2"), None);
        assert_eq!(TreeIdentityAlgorithm::from_id("tree-identity.v99"), None);
    }

    #[test]
    fn every_namespace_is_excluded_and_dot_is_escaped() {
        let excludes = TreeIdentityAlgorithm::V1.excludes();
        assert_eq!(excludes.len(), V1_NAMESPACES.len());
        for exclude in &excludes {
            assert!(exclude.starts_with(":(exclude,top,glob)[.]"), "{exclude}");
            assert!(exclude.ends_with("/**"), "{exclude}");
        }
    }

    #[test]
    fn config_paths_live_inside_an_excluded_namespace() {
        // Configuration outside every namespace would be included anyway, so
        // listing it would misstate why it is in the tree.
        let v1 = TreeIdentityAlgorithm::V1;
        for path in v1.config_paths() {
            assert!(
                v1.namespaces()
                    .iter()
                    .any(|namespace| path.starts_with(&format!("{namespace}/"))),
                "{path} is not inside a frozen namespace"
            );
        }
    }

    #[test]
    fn config_is_re_added_after_namespaces_are_dropped() {
        let plan = TreeIdentityAlgorithm::V1.plan();
        let drop_at = plan
            .iter()
            .position(|step| matches!(step, Step::DropNamespaces { .. }))
            .expect("plan drops namespaces");
        let config_at = plan
            .iter()
            .position(|step| matches!(step, Step::ForceAddConfig { .. }))
            .expect("plan force-adds config");
        assert!(drop_at < config_at, "config re-added before the drop");
    }
}
