use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use teko_protocol::WorkPacketEnvelope;

use crate::command::{CommandRunner, ProcessCommandRunner};

#[derive(Debug, Clone)]
pub struct ActiveWork {
    pub packet: WorkPacketEnvelope,
    pub status: Value,
}

pub fn has_active_work(repo_root: &Path) -> Result<bool> {
    Ok(active_work(repo_root)?.is_some())
}

pub fn active_work(repo_root: &Path) -> Result<Option<ActiveWork>> {
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
    Ok(Some(ActiveWork {
        packet,
        status: status.clone(),
    }))
}

pub fn active_work_packet(repo_root: &Path) -> Result<Option<String>> {
    Ok(active_work(repo_root)?.map(|active| render(&active)))
}

pub(crate) fn render(active: &ActiveWork) -> String {
    let packet = &active.packet;
    let mut lines = vec![
        "# Active Teko Work".to_string(),
        String::new(),
        format!("Work: `{}`", packet.work.id),
        format!("Spec: `{}` — {}", packet.spec.id, packet.spec.title),
        format!("Objective: {}", packet.work.objective),
        "Obligations:".into(),
    ];
    let states = active
        .status
        .pointer("/status/obligations")
        .and_then(Value::as_array);
    lines.extend(
        packet
            .obligations
            .iter()
            .enumerate()
            .map(|(index, obligation)| {
                let resolved = states
                    .and_then(|items| items.get(index))
                    .and_then(|item| item.get("resolved"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                format!(
                    "- [{}] `{}` {}",
                    if resolved { "done" } else { "open" },
                    obligation.id,
                    obligation.description
                )
            }),
    );
    if !packet.spec.non_goals.is_empty() {
        lines.push("Non-goals:".into());
        lines.extend(packet.spec.non_goals.iter().map(|item| format!("- {item}")));
    }
    lines.push("Write scope:".into());
    lines.extend(packet.scope.write.iter().map(|path| format!("- {path}")));
    lines.push(format!(
        "Completion required: {}",
        match packet.required_completion {
            teko_protocol::CompletionLevel::Changed => "changed",
            teko_protocol::CompletionLevel::Validated => "validated",
        }
    ));
    lines.push("Closure blockers:".into());
    let blockers = active
        .status
        .pointer("/status/blockers")
        .and_then(Value::as_array);
    match blockers {
        Some(items) if !items.is_empty() => lines.extend(items.iter().map(|blocker| {
            format!(
                "- `{}`: {}",
                blocker["code"].as_str().unwrap_or("unknown"),
                blocker["message"].as_str().unwrap_or("unknown")
            )
        })),
        _ => lines.push("- none".into()),
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::command::CommandOutput;

    struct FakeRunner {
        outputs: RefCell<Vec<CommandOutput>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "teko");
            assert!(matches!(args, ["current"] | ["packet", "--json"]));
            Ok(self.outputs.borrow_mut().remove(0))
        }
    }

    #[test]
    fn active_work_uses_the_versioned_packet_and_current_status() {
        let runner = FakeRunner {
            outputs: RefCell::new(vec![
                CommandOutput {
                    status: 0,
                    stdout: br#"{"active":{"status":{"obligations":[],"blockers":[]}}}"#.to_vec(),
                    stderr: Vec::new(),
                },
                CommandOutput {
                    status: 0,
                    stdout: teko_protocol::WORK_PACKET_FIXTURE.as_bytes().to_vec(),
                    stderr: Vec::new(),
                },
            ]),
        };
        let active = active_work_with(&runner, Path::new(".")).unwrap().unwrap();
        assert_eq!(active.packet.work.id.as_str(), "work_shared-semantic-v0");
        assert!(render(&active).contains("Shared semantic contract v0"));
    }

    #[test]
    fn no_active_work_does_not_request_a_packet() {
        let runner = FakeRunner {
            outputs: RefCell::new(vec![CommandOutput {
                status: 0,
                stdout: br#"{"active":null}"#.to_vec(),
                stderr: Vec::new(),
            }]),
        };
        assert!(active_work_with(&runner, Path::new(".")).unwrap().is_none());
    }
}
