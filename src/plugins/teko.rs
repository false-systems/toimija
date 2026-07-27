//! Teko plugin: durable bounded work.
//!
//! Consumes the standalone Teko CLI through its machine interface. `teko
//! current` always answers with one JSON envelope — `{"active": null}` when no
//! bounded work is active, otherwise the full status document under the
//! `active` key. The observation stored in the packet is the unwrapped
//! document, so the active Teko work travels with the session packet exactly
//! as Teko derived it.

use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use teko_protocol::WorkPacketEnvelope;

use crate::command::{CommandRunner, ProcessCommandRunner};
use crate::plugins::{Identity, Plugin, PluginMeta};

pub(crate) struct TekoPlugin;

impl Plugin for TekoPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "teko",
            description: "active bounded work: intent, write scope, and closure blockers",
            command: "teko",
            check: "teko show",
        }
    }

    fn observe(&self, repo_root: &Path) -> Result<Option<Value>> {
        active_work_value_with(&ProcessCommandRunner, repo_root)
    }

    fn identity(&self, observation: &Value) -> Option<Identity> {
        let id = observation.pointer("/work/id").and_then(Value::as_str)?;
        let objective = observation
            .pointer("/work/objective")
            .and_then(Value::as_str)?;
        let scope = observation
            .pointer("/work/scope/write")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|rule| rule["path"].as_str().map(str::to_owned))
            .collect();
        Some(Identity {
            intent: format!("teko:{id} — {objective}"),
            scope,
        })
    }

    fn render(&self, observation: &Value) -> String {
        render(observation)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveWork {
    pub packet: WorkPacketEnvelope,
    pub status: Value,
}

pub(crate) fn active_work(repo_root: &Path) -> Result<Option<ActiveWork>> {
    active_work_with(&ProcessCommandRunner, repo_root)
}

fn active_work_with(runner: &impl CommandRunner, repo_root: &Path) -> Result<Option<ActiveWork>> {
    let current = match runner.run(repo_root, "teko", &["current"]) {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to run `teko current`"),
    };
    if current.status != 0 {
        bail!("`teko current` failed: {}", current.stderr_lossy().trim());
    }
    let current: Value =
        serde_json::from_slice(&current.stdout).context("`teko current` returned invalid JSON")?;
    let Some(status) = current.get("active").filter(|value| !value.is_null()) else {
        return Ok(None);
    };

    let packet = runner
        .run(repo_root, "teko", &["packet", "--json"])
        .context("failed to run `teko packet --json`")?;
    if packet.status != 0 {
        bail!(
            "`teko packet --json` failed: {}",
            packet.stderr_lossy().trim()
        );
    }
    let packet: WorkPacketEnvelope = serde_json::from_slice(&packet.stdout)
        .context("`teko packet --json` returned an invalid work envelope")?;
    packet.validate().map_err(anyhow::Error::from)?;
    let status_work = status
        .pointer("/work/id")
        .and_then(Value::as_str)
        .context("`teko current` omitted the active work id")?;
    let status_spec = status
        .pointer("/spec/id")
        .and_then(Value::as_str)
        .context("`teko current` omitted the active spec id")?;
    if (packet.work.id.as_str() != status_work
        && packet.work.id.as_str().strip_prefix("work_") != Some(status_work))
        || (packet.spec.id.as_str() != status_spec
            && packet.spec.id.as_str().strip_prefix("spec_") != Some(status_spec))
    {
        bail!(
            "active Teko work changed while Toimija read it (status {status_work}/{status_spec}, packet {}/{}). Teko is authoritative for active work; retry context generation.",
            packet.work.id,
            packet.spec.id
        );
    }
    Ok(Some(ActiveWork {
        packet,
        status: status.clone(),
    }))
}

fn active_work_value_with(runner: &impl CommandRunner, repo_root: &Path) -> Result<Option<Value>> {
    let output = match runner.run(repo_root, "teko", &["current"]) {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to run `teko current`"),
    };
    if output.status != 0 {
        bail!("`teko current` failed: {}", output.stderr_lossy().trim());
    }
    parse_active_work(&output.stdout)
}

fn parse_active_work(output: &[u8]) -> Result<Option<Value>> {
    let value: Value =
        serde_json::from_slice(output).context("`teko current` returned invalid JSON")?;
    let Some(active) = value.get("active") else {
        bail!("`teko current` omitted the `active` envelope");
    };
    if active.is_null() {
        return Ok(None);
    }
    for pointer in [
        "/work/id",
        "/work/objective",
        "/spec/id",
        "/spec/title",
        "/status/required_completion",
    ] {
        if !active.pointer(pointer).is_some_and(Value::is_string) {
            bail!("`teko current` omitted required field `{pointer}`");
        }
    }
    let scope = active
        .pointer("/work/scope/write")
        .and_then(Value::as_array)
        .context("`teko current` omitted required field `/work/scope/write`")?;
    if scope.iter().any(|rule| !rule["path"].is_string()) {
        bail!("`teko current` returned an invalid write scope");
    }
    Ok(Some(active.clone()))
}

fn render(value: &Value) -> String {
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
        outputs: RefCell<Vec<std::io::Result<CommandOutput>>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "teko");
            assert!(matches!(args, ["current"] | ["packet", "--json"]));
            self.outputs.borrow_mut().remove(0)
        }
    }

    fn output(stdout: &[u8]) -> std::io::Result<CommandOutput> {
        Ok(CommandOutput {
            status: 0,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        })
    }

    #[test]
    fn active_work_is_read_from_the_teko_current_envelope() {
        let runner = FakeRunner {
            outputs: RefCell::new(vec![output(
                br#"{"active":{"work":{"id":"w","objective":"ship","scope":{"write":[{"path":"src"}]}},"spec":{"id":"s","title":"T","goals":[],"non_goals":[]},"status":{"required_completion":"validated","obligations":[],"blockers":[],"next_actions":[],"closable":true}}}"#,
            )]),
        };
        let value = active_work_value_with(&runner, Path::new("."))
            .unwrap()
            .unwrap();
        assert_eq!(value["work"]["objective"], "ship");
        let identity = TekoPlugin.identity(&value).unwrap();
        assert_eq!(identity.intent, "teko:w — ship");
        assert_eq!(identity.scope, ["src"]);
        assert!(TekoPlugin.render(&value).contains("Objective: ship"));
    }

    #[test]
    fn active_work_uses_the_versioned_packet_and_current_status() {
        let runner = FakeRunner {
            outputs: RefCell::new(vec![
                output(br#"{"active":{"work":{"id":"shared-semantic-v0"},"spec":{"id":"shared-semantic-v0"},"status":{"obligations":[],"blockers":[]}}}"#),
                output(teko_protocol::WORK_PACKET_FIXTURE.as_bytes()),
            ]),
        };
        let active = active_work_with(&runner, Path::new(".")).unwrap().unwrap();
        assert_eq!(active.packet.work.id.as_str(), "work_shared-semantic-v0");
    }

    #[test]
    fn active_work_refuses_a_packet_from_another_work_item() {
        let runner = FakeRunner {
            outputs: RefCell::new(vec![
                output(br#"{"active":{"work":{"id":"old"},"spec":{"id":"old"},"status":{}}}"#),
                output(teko_protocol::WORK_PACKET_FIXTURE.as_bytes()),
            ]),
        };

        assert!(
            active_work_with(&runner, Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("active Teko work changed")
        );
    }

    #[test]
    fn no_active_work_does_not_request_a_packet() {
        let runner = FakeRunner {
            outputs: RefCell::new(vec![output(br#"{"active":null}"#)]),
        };
        assert!(active_work_with(&runner, Path::new(".")).unwrap().is_none());
    }

    #[test]
    fn a_missing_teko_binary_disables_the_plugin_silently() {
        let runner = FakeRunner {
            outputs: RefCell::new(vec![Err(std::io::Error::from(ErrorKind::NotFound))]),
        };
        assert!(
            active_work_value_with(&runner, Path::new("."))
                .unwrap()
                .is_none()
        );
    }
}
