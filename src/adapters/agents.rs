//! Agent launch adapters (proposal 0006 Part C): argv composition for known
//! agent CLIs. The composed launch prompt is the one delivery channel with a
//! guarantee — the agent cannot not-see its own argv — so under `toimija run`
//! the packet is in context by construction, not by instruction. Pure string
//! work: no protocol, no IPC, no SDK.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptStyle {
    Positional,
}

/// Agent CLIs and their delivery style, matched by basename. Deliberately a
/// short explicit table; unknown agents keep the env-var handshake behind an
/// honest banner.
const KNOWN_AGENTS: &[(&str, PromptStyle)] = &[
    ("claude", PromptStyle::Positional),
    ("codex", PromptStyle::Positional),
    ("gemini", PromptStyle::Positional),
];

fn prompt_style(program: &str) -> Option<PromptStyle> {
    KNOWN_AGENTS
        .iter()
        .find(|(name, _)| *name == basename(program))
        .map(|(_, style)| *style)
}

pub fn is_known_agent(program: &str) -> bool {
    prompt_style(program).is_some()
}

fn basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
}

/// Compose the launch prompt for a known agent: contract framing, the entry
/// packet inlined (already rendered at the session budget), the live packet
/// path for re-reads, then the human's task. `None` for unknown agents.
pub fn compose_launch_prompt(
    program: &str,
    packet_markdown: &str,
    live_packet: &Path,
    task: Option<&str>,
) -> Option<String> {
    let style = prompt_style(program)?;
    let mut prompt = String::new();
    prompt.push_str(
        "Your toimija session packet is below. It is your workset contract: work from it, \
         and declare any expansion (`toimija intent \"<why>\" --scope <path>`) before \
         reading outside it.\n",
    );
    prompt.push_str(&format!(
        "Live packet (re-read after Git-state changes): {}\n\n",
        live_packet.display()
    ));
    prompt.push_str(packet_markdown.trim_end());
    prompt.push('\n');
    if let Some(task) = task {
        prompt.push_str(&format!("\nTask: {task}\n"));
    }
    match style {
        PromptStyle::Positional => Some(prompt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn known_agents_match_by_basename_only() {
        assert!(is_known_agent("claude"));
        assert!(is_known_agent("/usr/local/bin/codex"));
        assert!(is_known_agent("gemini"));
        assert!(!is_known_agent("sh"));
        assert!(!is_known_agent("/usr/bin/claudette"));
    }

    #[test]
    fn composed_prompt_carries_contract_packet_path_and_task() {
        let live = PathBuf::from(".toimija/sessions/1-1/current.md");
        let prompt = compose_launch_prompt(
            "codex",
            "# Toimija Context\n\nbody\n",
            &live,
            Some("fix #51"),
        )
        .unwrap();
        assert!(prompt.starts_with("Your toimija session packet is below."));
        assert!(prompt.contains("workset contract"));
        assert!(prompt.contains(".toimija/sessions/1-1/current.md"));
        assert!(prompt.contains("# Toimija Context"));
        assert!(prompt.ends_with("Task: fix #51\n"));
    }

    #[test]
    fn task_section_is_omitted_when_absent_and_unknown_agents_get_none() {
        let live = PathBuf::from("p.md");
        let prompt = compose_launch_prompt("claude", "packet", &live, None).unwrap();
        assert!(!prompt.contains("Task:"));
        assert!(compose_launch_prompt("bash", "packet", &live, None).is_none());
    }
}
