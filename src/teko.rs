use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::command::{CommandRunner, ProcessCommandRunner};

pub fn active_work_value(repo_root: &Path) -> Result<Option<Value>> {
    active_work_value_with(&ProcessCommandRunner, repo_root)
}

fn active_work_value_with(runner: &impl CommandRunner, repo_root: &Path) -> Result<Option<Value>> {
    let output = match runner.run(repo_root, "teko", &["show", "--json"]) {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to run `teko show --json`"),
    };
    if output.status != 0 {
        bail!(
            "`teko show --json` failed: {}",
            output.stderr_lossy().trim()
        );
    }
    parse_active_work(&output.stdout)
}

fn parse_active_work(output: &[u8]) -> Result<Option<Value>> {
    let value: Value =
        serde_json::from_slice(output).context("`teko show --json` returned invalid JSON")?;
    if value.pointer("/active").is_some_and(Value::is_null) {
        return Ok(None);
    }
    for pointer in [
        "/work/id",
        "/work/objective",
        "/spec/id",
        "/spec/title",
        "/status/required_completion",
    ] {
        if !value.pointer(pointer).is_some_and(Value::is_string) {
            bail!("`teko show --json` omitted required field `{pointer}`");
        }
    }
    let scope = value
        .pointer("/work/scope/write")
        .and_then(Value::as_array)
        .context("`teko show --json` omitted required field `/work/scope/write`")?;
    if scope.iter().any(|rule| !rule["path"].is_string()) {
        bail!("`teko show --json` returned an invalid write scope");
    }
    Ok(Some(value))
}

pub(crate) fn session_metadata(value: &Value) -> (String, Vec<String>) {
    let id = value.pointer("/work/id").and_then(Value::as_str).unwrap();
    let objective = value
        .pointer("/work/objective")
        .and_then(Value::as_str)
        .unwrap();
    let scope = value
        .pointer("/work/scope/write")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|rule| rule["path"].as_str().map(str::to_owned))
        .collect();
    (format!("teko:{id} — {objective}"), scope)
}

pub(crate) fn render(value: &Value) -> String {
    let text = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    };
    let mut lines = vec![
        "# Active Teko Work".to_string(),
        String::new(),
        format!("Work: `{}`", text("/work/id")),
        format!("Spec: `{}` — {}", text("/spec/id"), text("/spec/title")),
        format!("Objective: {}", text("/work/objective")),
        "Goals:".into(),
    ];
    if let Some(goals) = value.pointer("/spec/goals").and_then(Value::as_array) {
        lines.extend(goals.iter().map(|goal| {
            let id = goal["id"].as_str().unwrap_or("unknown");
            let description = goal["description"].as_str().unwrap_or("unknown");
            let resolved = value
                .pointer("/status/obligations")
                .and_then(Value::as_array)
                .and_then(|obligations| {
                    obligations
                        .iter()
                        .find(|obligation| obligation["id"].as_str() == Some(id))
                })
                .and_then(|obligation| obligation["resolved"].as_bool())
                .unwrap_or(false);
            format!(
                "- [{}] `{id}` {description}",
                if resolved { "done" } else { "open" }
            )
        }));
    }
    lines.push("Non-goals:".into());
    if let Some(non_goals) = value.pointer("/spec/non_goals").and_then(Value::as_array) {
        lines.extend(
            non_goals
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("- {item}")),
        );
    }
    lines.push("Write scope:".into());
    if let Some(scope) = value.pointer("/work/scope/write").and_then(Value::as_array) {
        lines.extend(
            scope
                .iter()
                .map(|rule| format!("- {}", rule["path"].as_str().unwrap_or("unknown"))),
        );
    }
    lines.push(format!(
        "Completion required: {}",
        text("/status/required_completion")
    ));
    lines.push("Closure blockers:".into());
    if let Some(blockers) = value.pointer("/status/blockers").and_then(Value::as_array) {
        if blockers.is_empty() {
            lines.push("- none".into());
        } else {
            lines.extend(blockers.iter().map(|blocker| {
                format!(
                    "- `{}`: {}",
                    blocker["code"].as_str().unwrap_or("unknown"),
                    blocker["message"].as_str().unwrap_or("unknown")
                )
            }));
        }
    }
    if let Some(next) = value
        .pointer("/status/next_actions/0/action")
        .and_then(Value::as_str)
    {
        lines.push(format!("Next: `{next}`"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::command::CommandOutput;

    struct FakeRunner {
        output: RefCell<Option<std::io::Result<CommandOutput>>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "teko");
            assert_eq!(args, ["show", "--json"]);
            self.output.borrow_mut().take().unwrap()
        }
    }

    #[test]
    fn active_work_is_read_from_the_standalone_teko_cli() {
        let runner = FakeRunner {
            output: RefCell::new(Some(Ok(CommandOutput {
                status: 0,
                stdout: br#"{"work":{"id":"w","objective":"ship","scope":{"write":[{"path":"src"}]}},"spec":{"id":"s","title":"T","goals":[],"non_goals":[]},"status":{"required_completion":"validated","obligations":[],"blockers":[],"next_actions":[],"closable":true}}"#.to_vec(),
                stderr: Vec::new(),
            }))),
        };
        let value = active_work_value_with(&runner, Path::new("."))
            .unwrap()
            .unwrap();
        assert_eq!(value["work"]["objective"], "ship");
        assert!(render(&value).contains("Objective: ship"));
    }

    #[test]
    fn no_active_work_is_distinct_from_an_invalid_contract() {
        assert!(parse_active_work(br#"{"active":null}"#).unwrap().is_none());
        assert!(parse_active_work(b"{}").is_err());
    }
}
