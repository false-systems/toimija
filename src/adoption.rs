//! Repository-local adoption: agent entry preambles and best-effort Git hooks.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tracing::info;

use crate::cli::Refusal;
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
    pub settings_written: bool,
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
    let hook_plan = if with_hooks {
        Some(preflight_hooks(repo_root)?)
    } else {
        None
    };

    for path in &targets {
        // A read failure (permissions, non-UTF-8) must abort, never default to
        // empty: writing the block over a defaulted-empty read would replace
        // the file's real content.
        let old = if path.is_file() {
            fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?
        } else {
            String::new()
        };
        let new = apply_block(&old)?;
        if new != old {
            fs::write(path, new).with_context(|| format!("failed to write {}", path.display()))?;
        }
    }

    let hooks = if with_hooks {
        let hooks_dir = &hook_plan.as_ref().expect("hooks requested").hooks_dir;
        install_hooks(hooks_dir)?
    } else {
        Vec::new()
    };
    let settings_written = if let Some(plan) = hook_plan {
        write_settings(&plan)?
    } else {
        false
    };
    info!(
        event = "toimija.adopt.applied",
        files = targets.len(),
        created_agents,
        hooks = hooks.len(),
        settings_written
    );
    Ok(AdoptionReport {
        files: targets,
        created_agents,
        hooks,
        settings_written,
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
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
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

/// Whitespace-insensitive, like the boundaries drift check: a formatter
/// re-wrapping the preamble line must not read as drift. `adopt` still writes
/// the canonical bytes; only the *check* tolerates re-wrapping.
fn has_current_block(text: &str) -> bool {
    let Some(start) = text.find(BEGIN) else {
        return false;
    };
    let after_begin = start + BEGIN.len();
    let Some(end) = text[after_begin..].find(END) else {
        return false;
    };
    normalized_ws(&text[after_begin..after_begin + end]) == normalized_ws(PREAMBLE)
}

fn normalized_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct HookPlan {
    hooks_dir: PathBuf,
    settings: SettingsUpdate,
}

struct SettingsUpdate {
    path: PathBuf,
    content: Option<Vec<u8>>,
}

fn preflight_hooks(repo_root: &Path) -> Result<HookPlan> {
    let runner = ProcessCommandRunner;
    let raw = git::run_git_ok(&runner, repo_root, &["rev-parse", "--git-path", "hooks"])
        .context("failed to locate the Git hooks directory")?;
    let hooks_dir = PathBuf::from(raw.trim());
    let hooks_dir = if hooks_dir.is_absolute() {
        hooks_dir
    } else {
        repo_root.join(hooks_dir)
    };
    scan_foreign_hooks(&hooks_dir)?;
    let settings = prepare_settings(repo_root)?;
    Ok(HookPlan {
        hooks_dir,
        settings,
    })
}

fn scan_foreign_hooks(hooks_dir: &Path) -> Result<()> {
    for name in HOOKS {
        let path = hooks_dir.join(name);
        if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read existing hook {}", path.display()))?;
            if !text.contains(HOOK_MARKER) {
                bail!(
                    "refusing to replace foreign Git hook {}. Add `toimija refresh >/dev/null 2>&1 || true` manually after scrubbing GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, and GIT_COMMON_DIR.",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn install_hooks(hooks_dir: &Path) -> Result<Vec<String>> {
    fs::create_dir_all(hooks_dir)
        .with_context(|| format!("failed to create {}", hooks_dir.display()))?;
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

fn prepare_settings(repo_root: &Path) -> Result<SettingsUpdate> {
    let path = repo_root.join(".claude/settings.json");
    if !path.exists() {
        return Ok(SettingsUpdate {
            path,
            content: Some(serialized_settings(&json!({
                "hooks": { "SessionStart": [session_start_hook()] }
            }))?),
        });
    }

    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut settings: Value = serde_json::from_slice(&bytes)
        .map_err(|error| malformed_settings(&path, format!("JSON parse error: {error}")))?;
    let object = settings
        .as_object_mut()
        .ok_or_else(|| malformed_settings(&path, "the top level must be a JSON object"))?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| malformed_settings(&path, "the `hooks` value must be an object"))?;
    let session_start = hooks
        .entry("SessionStart")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| {
            malformed_settings(&path, "the `hooks.SessionStart` value must be an array")
        })?;
    if session_start.iter().any(session_start_contains_toimija) {
        return Ok(SettingsUpdate {
            path,
            content: None,
        });
    }
    session_start.push(session_start_hook());
    Ok(SettingsUpdate {
        path,
        content: Some(serialized_settings(&settings)?),
    })
}

fn session_start_hook() -> Value {
    json!({
        "hooks": [{
            "type": "command",
            "command": "toimija context --budget 4000"
        }]
    })
}

fn session_start_contains_toimija(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
        .any(|command| command.contains("toimija context"))
}

fn serialized_settings(settings: &Value) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(settings).context("failed to render Claude settings")?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn malformed_settings(path: &Path, detail: impl Into<String>) -> anyhow::Error {
    anyhow!(Refusal {
        reason: "malformed-settings",
        message: format!(
            "toimija adopt --hooks: malformed-settings — cannot update {} because it is not usable JSON settings.\n{}\nFix that file manually, then rerun `toimija adopt --hooks`; it was not overwritten.",
            path.display(),
            detail.into()
        ),
    })
}

fn write_settings(plan: &HookPlan) -> Result<bool> {
    let Some(content) = &plan.settings.content else {
        return Ok(false);
    };
    if let Some(parent) = plan.settings.path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&plan.settings.path, content)
        .with_context(|| format!("failed to write {}", plan.settings.path.display()))?;
    Ok(true)
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

    #[test]
    fn rewrapped_preamble_still_counts_as_current() {
        let adopted = apply_block("# Guide\n").unwrap();
        assert!(has_current_block(&adopted));
        // A markdown formatter re-wraps the long preamble line; that is not drift.
        let rewrapped = adopted.replace("workset contract. Under", "workset contract.\nUnder");
        assert_ne!(adopted, rewrapped);
        assert!(has_current_block(&rewrapped));
        // A materially edited preamble is drift.
        let edited = adopted.replace("workset contract", "old contract");
        assert!(!has_current_block(&edited));
    }
}
