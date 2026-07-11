//! Repository-local adoption: agent entry preambles and best-effort Git hooks.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::info;

use crate::command::ProcessCommandRunner;
use crate::git;

pub const BEGIN: &str = "<!-- toimija:adopt:v1:begin -->";
pub const END: &str = "<!-- toimija:adopt:v1:end -->";
pub const PREAMBLE: &str = "Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.";

const ENTRY_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".cursorrules",
    ".github/copilot-instructions.md",
    "GEMINI.md",
    ".rules",
];
const HOOKS: &[&str] = &["post-checkout", "post-merge", "post-commit"];
const HOOK_MARKER: &str = "# toimija:adopt-hooks:v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionReport {
    pub files: Vec<PathBuf>,
    pub created_agents: bool,
    pub hooks: Vec<String>,
}

pub fn adopt(repo_root: &Path, with_hooks: bool) -> Result<AdoptionReport> {
    let existing: Vec<PathBuf> = ENTRY_FILES
        .iter()
        .map(|path| repo_root.join(path))
        .filter(|path| path.is_file())
        .collect();
    let (targets, created_agents) = if existing.is_empty() {
        (vec![repo_root.join("AGENTS.md")], true)
    } else {
        (existing, false)
    };

    for path in &targets {
        let old = fs::read_to_string(path).unwrap_or_default();
        let new = apply_block(&old)?;
        if new != old {
            fs::write(path, new).with_context(|| format!("failed to write {}", path.display()))?;
        }
    }

    let hooks = if with_hooks {
        install_hooks(repo_root)?
    } else {
        Vec::new()
    };
    info!(
        event = "toimija.adopt.applied",
        files = targets.len(),
        created_agents,
        hooks = hooks.len()
    );
    Ok(AdoptionReport {
        files: targets,
        created_agents,
        hooks,
    })
}

pub fn adoption_status(repo_root: &Path) -> Result<(usize, Vec<PathBuf>)> {
    let mut present = 0;
    let mut stale = Vec::new();
    for relative in ENTRY_FILES {
        let path = repo_root.join(relative);
        if !path.is_file() {
            continue;
        }
        present += 1;
        let text = fs::read_to_string(&path).unwrap_or_default();
        if !has_current_block(&text) {
            stale.push(PathBuf::from(relative));
        }
    }
    Ok((present, stale))
}

fn apply_block(text: &str) -> Result<String> {
    let block = format!("{BEGIN}\n{PREAMBLE}\n{END}");
    let without = match (text.find(BEGIN), text.find(END)) {
        (None, None) => text.to_string(),
        (Some(start), Some(end)) if end >= start => {
            let end = end + END.len();
            return Ok(format!("{}{}{}", &text[..start], block, &text[end..]));
        }
        _ => bail!("malformed toimija adoption markers"),
    };
    let (prefix, suffix) = if let Some(first) = without.lines().next() {
        if first.trim_start().starts_with('#') {
            let end = first.len();
            (&without[..end], &without[end..])
        } else {
            ("", without.as_str())
        }
    } else {
        ("", without.as_str())
    };
    let suffix = suffix.trim_matches(['\n', '\r']);
    let mut output = String::new();
    if !prefix.is_empty() {
        output.push_str(prefix);
        output.push_str("\n\n");
    }
    output.push_str(&block);
    if !suffix.is_empty() {
        output.push_str("\n\n");
        output.push_str(suffix);
    }
    output.push('\n');
    Ok(output)
}

fn has_current_block(text: &str) -> bool {
    let Some(start) = text.find(BEGIN) else {
        return false;
    };
    let Some(end) = text.find(END) else {
        return false;
    };
    end > start && text[start..=end + END.len() - 1] == format!("{BEGIN}\n{PREAMBLE}\n{END}")
}

fn install_hooks(repo_root: &Path) -> Result<Vec<String>> {
    let runner = ProcessCommandRunner;
    let raw = git::run_git_ok(&runner, repo_root, &["rev-parse", "--git-path", "hooks"])
        .context("failed to locate the Git hooks directory")?;
    let hooks_dir = PathBuf::from(raw.trim());
    let hooks_dir = if hooks_dir.is_absolute() {
        hooks_dir
    } else {
        repo_root.join(hooks_dir)
    };
    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("failed to create {}", hooks_dir.display()))?;
    for name in HOOKS {
        let path = hooks_dir.join(name);
        if path.exists() {
            let text = fs::read_to_string(&path).unwrap_or_default();
            if !text.contains(HOOK_MARKER) {
                bail!(
                    "refusing to replace foreign Git hook {}. Add `toimija refresh >/dev/null 2>&1 || true` manually after scrubbing GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, and GIT_COMMON_DIR.",
                    path.display()
                );
            }
        }
    }
    let mut installed = Vec::new();
    for name in HOOKS {
        let path = hooks_dir.join(name);
        fs::write(&path, hook_script())
            .with_context(|| format!("failed to write {}", path.display()))?;
        make_executable(&path)?;
        installed.push((*name).to_string());
    }
    Ok(installed)
}

fn hook_script() -> &'static str {
    "#!/bin/sh\n# toimija:adopt-hooks:v1\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_NAMESPACE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX GIT_CEILING_DIRECTORIES\ntoimija refresh >/dev/null 2>&1 || true\nexit 0\n"
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

pub fn harness_snippet() -> &'static str {
    "For you to place in a Claude Code SessionStart hook (Toimija does not install this outside the repo):\n  toimija context --budget 4000"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_is_idempotent_and_follows_a_title() {
        let first = apply_block("# Guide\n\nKeep this.\n").unwrap();
        assert_eq!(apply_block(&first).unwrap(), first);
        assert!(first.starts_with("# Guide\n\n<!-- toimija:adopt:v1:begin -->"));
        assert_eq!(first.matches(BEGIN).count(), 1);
    }

    #[test]
    fn malformed_markers_are_rejected() {
        assert!(apply_block("<!-- toimija:adopt:v1:begin -->\n").is_err());
    }
}
