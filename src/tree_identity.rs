//! `tree-identity.v1`: which content makes up one version of a repository's
//! work.
//!
//! The identity is the Git tree of every tracked and untracked non-ignored
//! path, minus the namespaces of generated tool state frozen into the version,
//! plus the tool configuration it forces in. [`TreeIdentityAlgorithm::plan`]
//! returns the procedure as data, and a caller runs the git steps with its own
//! runner (`crate::snapshot` does). This module never executes anything, reads
//! a repository or stores state.
//!
//! An artifact records the algorithm's [`id`](TreeIdentityAlgorithm::id), so a
//! reader recomputes the identity under the rules that applied when it was
//! written. The rule is stated for readers in `docs/reference.md`,
//! "tree-identity.v1".

/// A versioned procedure for computing repository tree identity.
///
/// The algorithm is part of the contract. Its inputs — which namespaces are
/// excluded, which configuration is forced in, the order of the git steps — are
/// frozen per version. Changing any of them changes what recorded artifacts
/// mean, so it requires a new variant rather than an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeIdentityAlgorithm {
    /// `tree-identity.v1`.
    V1,
}

/// Namespaces excluded by `tree-identity.v1`. **Frozen.**
///
/// A namespace is here because its owner generates repository-local derived
/// state whose mutation must not change what work is being identified. Under
/// this algorithm version, the contents of these directories are defined as
/// generated operational state.
///
/// Speculative reservation is forbidden. Excluding a namespace nobody writes to
/// would silently erase meaningful content a developer later puts there, which
/// is strictly worse than carrying no entry at all.
///
/// **The set never grows.** There is no dynamic registration: reading namespaces
/// from configuration would let today's setup reinterpret yesterday's
/// identities. A tool that needs a further exclusion is a `V2` decision.
const V1_NAMESPACES: &[&str] = &[".toimija", ".teko", ".sykli"];

/// Configuration paths force-included by `tree-identity.v1`. **Frozen**, for
/// the same reason as [`V1_NAMESPACES`].
const V1_CONFIG_PATHS: &[&str] = &[".toimija/repo.yaml"];

impl TreeIdentityAlgorithm {
    /// Stable identifier recorded in artifacts.
    pub const fn id(self) -> &'static str {
        match self {
            Self::V1 => "tree-identity.v1",
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
    /// Returned as data rather than executed, so the caller runs it with its
    /// own process runner and error handling, and a test can run the same git
    /// operations in the same order without Toimija's code.
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
}

/// One step of a tree-identity procedure.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The test that makes "frozen" enforceable. If you are here because this
    /// failed, you changed `tree-identity.v1`. Add `V2` instead: editing v1
    /// silently reinterprets every tree identity recorded under it.
    #[test]
    fn v1_definition_is_frozen() {
        let v1 = TreeIdentityAlgorithm::V1;
        assert_eq!(v1.id(), "tree-identity.v1");
        assert_eq!(v1.namespaces(), &[".toimija", ".teko", ".sykli"]);
        assert_eq!(v1.config_paths(), &[".toimija/repo.yaml"]);
        let text = |items: &[&str]| items.iter().map(|item| item.to_string()).collect();
        assert_eq!(
            v1.plan(),
            vec![
                Step::SeedFromHead,
                Step::OverlayWorktree {
                    excludes: text(&[
                        ":(exclude,top,glob)[.]toimija/**",
                        ":(exclude,top,glob)[.]teko/**",
                        ":(exclude,top,glob)[.]sykli/**",
                    ]),
                },
                Step::DropNamespaces {
                    paths: text(&[".toimija", ".teko", ".sykli"]),
                },
                Step::ForceAddConfig {
                    path: ".toimija/repo.yaml".to_string(),
                },
                Step::WriteTree,
            ]
        );
    }

    /// The other half of the rule. A directory nobody reserved is ordinary
    /// work content, and must be treated as such — this is what speculative
    /// reservation would silently break.
    #[test]
    fn an_unreserved_namespace_is_ordinary_work_content() {
        let v1 = TreeIdentityAlgorithm::V1;
        for candidate in [".widget", ".example", ".acme", ".github"] {
            assert!(
                !v1.namespaces().contains(&candidate),
                "{candidate} is reserved without demonstrated generated state"
            );
        }
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
