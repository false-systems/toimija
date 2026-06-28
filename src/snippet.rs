//! Copy-pasteable text Toimija *suggests* but does not own. Each snippet is
//! printed to stdout for a human to paste into their own `AGENTS.md`,
//! `CLAUDE.md`, or a repo script. Toimija never writes these files itself —
//! root agent policy is human-owned; Toimija only owns `.toimija/`.

use serde::{Deserialize, Serialize};

/// Which snippet to print. Stable kebab-case names are the CLI surface
/// (`toimija snippet preflight`), so renaming a variant is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Snippet {
    Preflight,
    Script,
}

impl Snippet {
    pub fn text(self) -> &'static str {
        match self {
            Snippet::Preflight => PREFLIGHT,
            Snippet::Script => SCRIPT,
        }
    }
}

/// The agent preflight: paste into an agent prompt, `AGENTS.md`, or `CLAUDE.md`.
/// It points the agent at `toimija enter` as the single entry command and makes
/// the packet — not the prompt — the carrier of repo context.
pub const PREFLIGHT: &str = "\
Before editing, run `toimija enter --for-diff --budget 4000`.
Use its output as the working state: current Git state, changed surface, scope,
read order, coupling, risk, validation plan, repo boundaries, and safe-to-ignore.
If `$TOIMIJA_LIVE` is set, you are running under `toimija run`: that file is your
continuously-refreshed packet — re-read it after Git-state changes instead of
re-running `enter`.
Do not broad-crawl the repo unless the packet is insufficient.
If the packet contradicts the task, stop and report.
Do not claim completion without running `toimija validate --plan`.
";

/// A repo-owned entry script. Toimija prints it; a human may save it as
/// `scripts/agent-enter` and `chmod +x` it. Toimija does not create it.
pub const SCRIPT: &str = "\
#!/usr/bin/env bash
set -euo pipefail

# Agent entry: print the current repo context packet, then the validation plan.
toimija enter --for-diff --budget \"${TOIMIJA_BUDGET:-4000}\"
toimija validate --plan
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_routes_through_enter_and_validate() {
        let text = Snippet::Preflight.text();
        assert!(text.contains("toimija enter --for-diff --budget 4000"));
        assert!(text.contains("toimija validate --plan"));
    }

    #[test]
    fn preflight_points_a_wrapped_agent_at_the_live_packet() {
        // Under `toimija run`, the handshake env var is the agent's cue to re-read
        // the live packet rather than re-run `enter`.
        let text = Snippet::Preflight.text();
        assert!(text.contains("$TOIMIJA_LIVE"));
    }

    #[test]
    fn script_is_a_runnable_bash_stub() {
        let text = Snippet::Script.text();
        assert!(text.starts_with("#!/usr/bin/env bash"));
        assert!(text.contains("set -euo pipefail"));
        assert!(text.contains("toimija enter"));
        assert!(text.contains("toimija validate --plan"));
    }
}
