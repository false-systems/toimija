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

/// What `ohjaa check` said of the knowledge here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Knowledge {
    /// Its summary line, as ohjaa wrote it.
    pub summary: String,
    /// What it flagged as maybe no longer holding, a line each.
    pub flags: Vec<String>,
}

/// What both tools said, or why one said nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub agents: Result<Agents, String>,
    pub knowledge: Result<Knowledge, String>,
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

fn read_knowledge(root: &Path) -> Result<Knowledge, String> {
    let (code, out) = run("ohjaa", &["check"], root)?;
    knowledge_from(&out).ok_or_else(|| format!("ohjaa check gave no summary (exit {code})"))
}

/// The `summary` line of `ohjaa check` and its `flag` lines, without
/// their labels.
fn knowledge_from(out: &str) -> Option<Knowledge> {
    let labelled = |label: &'static str| {
        out.lines()
            .filter_map(move |line| line.strip_prefix(label))
            .map(|rest| rest.trim().to_string())
            .filter(|rest| !rest.is_empty())
    };
    Some(Knowledge {
        summary: labelled("summary").next()?,
        flags: labelled("flag").collect(),
    })
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

/// A length of time short enough for a column.
pub fn ago(seconds: u64) -> String {
    match seconds {
        0..=119 => "now".to_string(),
        120..=3_599 => format!("{}m", seconds / 60),
        3_600..=172_799 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// How long ago a session last wrote, as of `now` in Unix seconds.
pub fn age(last_at: Option<&str>, now: u64) -> String {
    match last_at.and_then(unix_seconds) {
        Some(at) => ago(now.saturating_sub(at)),
        None => "?".to_string(),
    }
}

/// Where a session works: the folder's own name.
pub fn place(cwd: Option<&str>) -> String {
    cwd.and_then(|cwd| Path::new(cwd).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".to_string())
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
    fn a_partial_read_is_marked_and_a_failure_is_not_an_empty_day() {
        let whole = agents_from(0, DIGEST).expect("the digest is read");
        assert!(!whole.partial);
        assert_eq!(whole.digest.sessions.len(), 3);
        assert_eq!(whole.digest.by_agent[1].output_tokens, None);
        assert!(
            agents_from(4, DIGEST)
                .expect("partial numbers are kept")
                .partial
        );
        assert_eq!(
            agents_from(1, "").unwrap_err(),
            "vessa digest failed (exit 1)"
        );
        assert!(agents_from(0, "not json").is_err());
        assert_eq!(place(Some("/work/alpha")), "alpha");
        assert_eq!(place(None), "?");
    }

    #[test]
    fn ohjaas_summary_and_what_it_flagged_are_kept_as_it_wrote_them() {
        let out = "scope    mine  /somewhere\nflag     one-rule: names a thing that is gone\n\
                   summary  3 current, 1 waiting for a decision\n";
        assert_eq!(
            knowledge_from(out),
            Some(Knowledge {
                summary: "3 current, 1 waiting for a decision".to_string(),
                flags: vec!["one-rule: names a thing that is gone".to_string()],
            })
        );
        assert_eq!(knowledge_from("scope  mine\n"), None);
        assert_eq!(ago(60), "now");
        assert_eq!(ago(7_200), "2h");
    }
}
