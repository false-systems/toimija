//! What the other tools know, for the pane: the agent sessions vessa has
//! read and what ohjaa says of the knowledge here.
//!
//! Each is asked through its own command and may be absent: a tool that is
//! not installed, fails or reads only part is said so, never shown as none.
//! Nothing here reads a conversation: vessa gives numbers and places only.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

/// The part of `vessa digest --json` the pane shows.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Digest {
    /// One entry for each agent session running now.
    #[serde(default)]
    pub live: Vec<String>,
    #[serde(default)]
    pub by_agent: Vec<ByAgent>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub warnings: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ByAgent {
    pub agent: String,
    /// Unknown where the agent records none.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub sessions: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub agent: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub last_at: Option<String>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub subagents: Option<u64>,
}

/// What vessa said, and whether it said it could not read everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agents {
    pub digest: Digest,
    pub partial: bool,
}

/// What both tools said, or why one said nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub agents: Result<Agents, String>,
    /// ohjaa's own summary line, as it wrote it.
    pub knowledge: Result<String, String>,
}

impl Facts {
    /// Ask both tools. Takes seconds: vessa collects first when its snapshot
    /// is old. Call it off the thread that draws.
    pub fn read(root: &Path) -> Self {
        Self {
            agents: read_agents(root),
            knowledge: read_knowledge(root),
        }
    }
}

fn run(tool: &str, args: &[&str], root: &Path) -> Result<(i32, String), String> {
    let output = Command::new(tool)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("{tool} is not installed"),
            _ => format!("{tool} could not be started: {error}"),
        })?;
    let code = output.status.code().unwrap_or(-1);
    Ok((code, String::from_utf8_lossy(&output.stdout).into_owned()))
}

fn read_agents(root: &Path) -> Result<Agents, String> {
    let (code, out) = run("vessa", &["digest", "--since", "24h", "--json"], root)?;
    agents_from(code, &out)
}

/// vessa exits 4 when it read only part: its numbers are kept and marked.
fn agents_from(code: i32, out: &str) -> Result<Agents, String> {
    if code != 0 && code != 4 {
        return Err(format!("vessa digest failed (exit {code})"));
    }
    let digest: Digest = serde_json::from_str(out)
        .map_err(|error| format!("vessa digest was not understood: {error}"))?;
    let partial = code == 4 || !digest.warnings.is_empty();
    Ok(Agents { digest, partial })
}

fn read_knowledge(root: &Path) -> Result<String, String> {
    let (code, out) = run("ohjaa", &["check"], root)?;
    summary_from(&out).ok_or_else(|| format!("ohjaa check gave no summary (exit {code})"))
}

/// The `summary` line of `ohjaa check`, without its label.
fn summary_from(out: &str) -> Option<String> {
    out.lines()
        .find_map(|line| line.strip_prefix("summary"))
        .map(|rest| rest.trim().to_string())
        .filter(|rest| !rest.is_empty())
}

/// A token count short enough for a column. Unknown stays unknown.
pub fn tokens(count: Option<u64>) -> String {
    match count {
        None => "?".to_string(),
        Some(n) if n < 1_000 => n.to_string(),
        Some(n) if n < 1_000_000 => format!("{}k", n / 1_000),
        Some(n) => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// Unix seconds of a UTC time as vessa writes it (`2026-11-26T12:50:25…Z`
/// or `…+00:00`). Any other offset is not guessed at.
fn unix_seconds(stamp: &str) -> Option<u64> {
    let utc = stamp.ends_with('Z') || stamp.ends_with("+00:00");
    let bytes = stamp.as_bytes();
    if !utc || bytes.len() < 20 || bytes[10] != b'T' {
        return None;
    }
    let number = |from: usize, to: usize| stamp.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    // Days from the civil date (proleptic Gregorian).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

/// How long ago a session last wrote, as of `now` in Unix seconds.
pub fn age(last_at: Option<&str>, now: u64) -> String {
    let Some(at) = last_at.and_then(unix_seconds) else {
        return "?".to_string();
    };
    let ago = now.saturating_sub(at);
    match ago {
        0..=119 => "now".to_string(),
        120..=3_599 => format!("{}m", ago / 60),
        3_600..=172_799 => format!("{}h", ago / 3_600),
        _ => format!("{}d", ago / 86_400),
    }
}

fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut short: String = text.chars().take(width.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// Where a session works: the folder's own name.
fn place(cwd: Option<&str>) -> String {
    cwd.and_then(|cwd| Path::new(cwd).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".to_string())
}

/// The sessions of the last day, the latest first, at most `limit` of them:
/// agent, folder, how long ago it last wrote, the tokens it wrote.
pub fn agents_lines(agents: &Agents, now: u64, width: usize, limit: usize) -> Vec<String> {
    let digest = &agents.digest;
    let mut lines = vec![format!(
        "{} live, {} in the last day",
        digest.live.len(),
        digest.sessions.len()
    )];
    let mut sessions: Vec<&Session> = digest.sessions.iter().collect();
    sessions.sort_by(|a, b| b.last_at.cmp(&a.last_at));
    // agent(8) folder(rest) age(4) tokens(6), a space between each.
    let folder = width.saturating_sub(8 + 4 + 6 + 3).max(4);
    for session in sessions.iter().take(limit) {
        lines.push(format!(
            "{:<8} {:<folder$} {:>4} {:>6}",
            cut(&session.agent, 8),
            cut(&place(session.cwd.as_deref()), folder),
            age(session.last_at.as_deref(), now),
            tokens(session.output_tokens),
        ));
    }
    if sessions.len() > limit {
        lines.push(format!("… {} more, 4 for all", sessions.len() - limit));
    }
    if agents.partial {
        lines.push("partial: vessa could not read it all".to_string());
    }
    lines
}

/// Tokens read and written by each agent over the last day.
pub fn totals_lines(agents: &Agents) -> Vec<String> {
    let mut lines = vec![format!(
        "{:<8} {:>8} {:>7} {:>7}",
        "", "sessions", "in", "out"
    )];
    for by in &agents.digest.by_agent {
        lines.push(format!(
            "{:<8} {:>8} {:>7} {:>7}",
            cut(&by.agent, 8),
            by.sessions
                .map_or_else(|| "?".to_string(), |n| n.to_string()),
            tokens(by.input_tokens),
            tokens(by.output_tokens),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = r#"{
        "live": ["claude"],
        "by_agent": [
            {"agent": "claude", "input_tokens": 1200, "output_tokens": 2500000, "sessions": 2},
            {"agent": "opencode", "input_tokens": null, "output_tokens": null, "sessions": 1}
        ],
        "sessions": [
            {"agent": "opencode", "cwd": "/work/alpha", "last_at": "2026-01-02T00:00:00Z",
             "output_tokens": null, "subagents": 0},
            {"agent": "claude", "cwd": "/work/a-very-long-folder-name", "last_at": "2026-01-02T11:59:30.250+00:00",
             "output_tokens": 2500000, "subagents": 3},
            {"agent": "claude", "cwd": "/work/beta", "last_at": "2026-01-02T11:00:00Z",
             "output_tokens": 900, "subagents": 0}
        ],
        "warnings": []
    }"#;
    /// 2026-01-02T12:00:00Z.
    const NOON: u64 = 1_767_355_200;

    #[test]
    fn a_time_is_read_as_utc_and_another_offset_is_not_guessed() {
        assert_eq!(unix_seconds("2026-01-02T12:00:00Z"), Some(NOON));
        assert_eq!(unix_seconds("2026-01-02T12:00:00.123+00:00"), Some(NOON));
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2026-01-02T12:00:00+02:00"), None);
        assert_eq!(unix_seconds("yesterday"), None);
    }

    #[test]
    fn an_age_is_short_and_an_unreadable_time_is_unknown() {
        assert_eq!(age(Some("2026-01-02T11:59:30Z"), NOON), "now");
        assert_eq!(age(Some("2026-01-02T11:00:00Z"), NOON), "1h");
        assert_eq!(age(Some("2026-01-02T11:30:00Z"), NOON), "30m");
        assert_eq!(age(Some("2025-12-29T12:00:00Z"), NOON), "4d");
        assert_eq!(age(None, NOON), "?");
        assert_eq!(age(Some("soon"), NOON), "?");
    }

    #[test]
    fn unknown_tokens_stay_unknown_and_never_become_zero() {
        assert_eq!(tokens(None), "?");
        assert_eq!(tokens(Some(0)), "0");
        assert_eq!(tokens(Some(999)), "999");
        assert_eq!(tokens(Some(17_256)), "17k");
        assert_eq!(tokens(Some(7_856_574)), "7.9M");
    }

    #[test]
    fn sessions_come_latest_first_each_within_the_pane() {
        let agents = agents_from(0, DIGEST).expect("the digest is read");
        assert!(!agents.partial);
        let lines = agents_lines(&agents, NOON, 38, 2);
        assert_eq!(lines[0], "1 live, 3 in the last day");
        assert!(
            lines[1].starts_with("claude   a-very-long-fold…"),
            "{lines:?}"
        );
        assert!(lines[1].ends_with(" now   2.5M"), "{lines:?}");
        assert!(lines[2].starts_with("claude   beta"), "{lines:?}");
        assert!(lines[2].ends_with("  1h    900"), "{lines:?}");
        assert_eq!(lines[3], "… 1 more, 4 for all");
        assert!(
            lines.iter().all(|line| line.chars().count() <= 38),
            "{lines:?}"
        );
        // The agent that records no tokens shows unknown, not zero.
        let all = agents_lines(&agents, NOON, 38, 9);
        assert!(all[3].ends_with("12h      ?"), "{all:?}");
        let totals = totals_lines(&agents);
        assert!(
            totals[2].ends_with("       1       ?       ?"),
            "{totals:?}"
        );
    }

    #[test]
    fn a_partial_read_is_said_and_a_failure_is_not_an_empty_day() {
        let partial = agents_from(4, DIGEST).expect("partial numbers are kept");
        assert!(partial.partial);
        let lines = agents_lines(&partial, NOON, 38, 9);
        assert_eq!(
            lines.last().map(String::as_str),
            Some("partial: vessa could not read it all")
        );
        assert_eq!(
            agents_from(1, "").unwrap_err(),
            "vessa digest failed (exit 1)"
        );
        assert!(agents_from(0, "not json").is_err());
    }

    #[test]
    fn ohjaas_summary_is_shown_as_it_wrote_it() {
        let out = "scope    mine  /somewhere\nsummary  3 current, 1 waiting for a decision\n";
        assert_eq!(
            summary_from(out).as_deref(),
            Some("3 current, 1 waiting for a decision")
        );
        assert_eq!(summary_from("scope  mine\n"), None);
    }
}
