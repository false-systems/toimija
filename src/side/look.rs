//! How the pane looks: text in tones, cards, bars, and the four views built
//! from them.
//!
//! A line is cut and padded as plain characters first and only then given
//! its colours, so a colour never counts as width. With `COLORTERM` naming
//! true colour the tones are exact; otherwise they are the terminal's own
//! sixteen.

use crate::graph::{Graph, Line as GraphLine, Proof, Review};
use crate::project::{Row, View};

use super::tools::{self, Facts};

/// What a piece of text is, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Bold,
    Dim,
    Good,
    Warn,
    Bad,
    /// The one loud colour: the name, the pick, a live mark.
    Accent,
    /// Branch and repository names.
    Cool,
    /// The view on screen, in the row of views.
    Pill,
    /// A step along a bar, from 0 at its start to 255 at its end.
    Heat(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    True,
    Basic,
}

impl Palette {
    pub fn detect() -> Self {
        match std::env::var("COLORTERM").as_deref() {
            Ok("truecolor" | "24bit") => Palette::True,
            _ => Palette::Basic,
        }
    }

    fn sgr(self, tone: Tone) -> String {
        match (self, tone) {
            (_, Tone::Plain) => String::new(),
            (_, Tone::Bold) => "1".to_string(),
            (Palette::True, Tone::Dim) => "38;2;112;118;132".to_string(),
            (Palette::True, Tone::Good) => "38;2;72;213;130".to_string(),
            (Palette::True, Tone::Warn) => "38;2;247;190;74".to_string(),
            (Palette::True, Tone::Bad) => "1;38;2;255;92;92".to_string(),
            (Palette::True, Tone::Accent) => "1;38;2;255;40;20".to_string(),
            (Palette::True, Tone::Cool) => "38;2;122;184;255".to_string(),
            (Palette::True, Tone::Pill) => "1;38;2;255;255;255;48;2;214;30;16".to_string(),
            (Palette::True, Tone::Heat(at)) => {
                // Red at the start of the bar, amber at its end.
                let at = u32::from(at);
                format!("38;2;255;{};{}", 40 + at * 160 / 255, 20 + at * 50 / 255)
            }
            (Palette::Basic, Tone::Dim) => "2".to_string(),
            (Palette::Basic, Tone::Good) => "32".to_string(),
            (Palette::Basic, Tone::Warn) => "33".to_string(),
            (Palette::Basic, Tone::Bad | Tone::Accent) => "1;31".to_string(),
            (Palette::Basic, Tone::Cool) => "36".to_string(),
            (Palette::Basic, Tone::Pill) => "1;7".to_string(),
            (Palette::Basic, Tone::Heat(_)) => "31".to_string(),
        }
    }
}

/// One line of the pane: pieces of text, each in a tone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text(Vec<(Tone, String)>);

impl Text {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn of(tone: Tone, text: impl Into<String>) -> Self {
        Self::new().add(tone, text)
    }

    pub fn add(mut self, tone: Tone, text: impl Into<String>) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.0.push((tone, text));
        }
        self
    }

    pub fn then(mut self, other: Text) -> Self {
        self.0.extend(other.0);
        self
    }

    pub fn width(&self) -> usize {
        self.0.iter().map(|(_, text)| text.chars().count()).sum()
    }

    /// The characters without their tones.
    #[cfg(test)]
    pub fn plain(&self) -> String {
        self.0.iter().map(|(_, text)| text.as_str()).collect()
    }

    /// Exactly `width` columns: cut where it is longer, padded where shorter.
    pub fn fit(self, width: usize) -> Self {
        let mut left = width;
        let mut fitted = Text::new();
        for (tone, text) in self.0 {
            if left == 0 {
                break;
            }
            let part: String = text.chars().take(left).collect();
            left -= part.chars().count();
            fitted = fitted.add(tone, part);
        }
        fitted.add(Tone::Plain, " ".repeat(left))
    }

    pub fn ansi(&self, palette: Palette) -> String {
        let mut out = String::new();
        for (tone, text) in &self.0 {
            let sgr = palette.sgr(*tone);
            if sgr.is_empty() {
                out.push_str(text);
            } else {
                out.push_str(&format!("\x1b[{sgr}m{text}\x1b[0m"));
            }
        }
        out
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

/// `left` and `right` at the two ends of `width` columns. The left gives way.
fn ends(left: Text, right: Text, width: usize) -> Text {
    let room = width.saturating_sub(right.width() + usize::from(right.width() > 0));
    left.fit(room)
        .add(Tone::Plain, if right.width() > 0 { " " } else { "" })
        .then(right)
        .fit(width)
}

/// Words over as many lines as they need, none wider than `width`.
fn wrapped(tone: Tone, text: &str, width: usize) -> Vec<Text> {
    crate::project::wrap(text, "", width)
        .into_iter()
        .map(|line| Text::of(tone, line))
        .collect()
}

/// A rounded box `width` columns wide: the title in its top edge, a note at
/// the far end of that edge, and each row inside.
pub fn card(title: &str, note: Text, rows: Vec<Text>, width: usize) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let note_width = if note.width() > 0 {
        note.width() + 2
    } else {
        0
    };
    let rule = width.saturating_sub(3 + title.chars().count() + 1 + note_width + 2);
    let mut top = Text::of(Tone::Dim, "╭─ ")
        .add(Tone::Bold, title)
        .add(Tone::Dim, format!(" {}", "─".repeat(rule)));
    if note.width() > 0 {
        top = top.add(Tone::Plain, " ").then(note).add(Tone::Plain, " ");
    }
    let mut lines = vec![top.add(Tone::Dim, "─╮").fit(width)];
    for row in rows {
        lines.push(
            Text::of(Tone::Dim, "│ ")
                .then(row.fit(inner))
                .add(Tone::Dim, " │"),
        );
    }
    lines.push(Text::of(
        Tone::Dim,
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
    ));
    lines
}

/// A thin bar `cells` wide, `part` of it lit from red to amber. Anything
/// above nothing lights at least one cell.
fn bar(part: f64, cells: usize) -> Text {
    let lit = if part <= 0.0 {
        0
    } else {
        ((part.min(1.0) * cells as f64).round() as usize).clamp(1, cells)
    };
    let mut text = Text::new();
    for cell in 0..lit {
        let heat = (cell * 255 / cells.max(1)) as u8;
        text = text.add(Tone::Heat(heat), "━");
    }
    text.add(Tone::Dim, "━".repeat(cells - lit))
}

/// The mark for what sykli says of a repository's branch.
fn proof_mark(proof: Option<&str>) -> Text {
    match proof {
        Some("proven") => Text::of(Tone::Good, "✓"),
        Some("failed") => Text::of(Tone::Bad, "✗"),
        None | Some("unknown") => Text::of(Tone::Warn, "?"),
        Some(_) => Text::of(Tone::Warn, "○"),
    }
}

/// What the marks mean, for the row above the keys.
pub fn legend() -> Text {
    Text::of(Tone::Good, "✓")
        .add(Tone::Dim, " proven ")
        .add(Tone::Bad, "✗")
        .add(Tone::Dim, " failed ")
        .add(Tone::Warn, "○")
        .add(Tone::Dim, " not run ")
        .add(Tone::Warn, "?")
        .add(Tone::Dim, " unknown")
}

/// The top line: the name, what is shown, and the time.
pub fn header(title: &str, clock: &str, width: usize) -> Text {
    ends(
        Text::of(Tone::Accent, "▍TOIMIJA")
            .add(Tone::Plain, "  ")
            .add(Tone::Bold, title),
        Text::of(Tone::Dim, clock),
        width,
    )
}

/// The views by number, the one on screen lit.
pub fn tabs(on: usize) -> Text {
    let mut text = Text::new();
    for (at, name) in ["projects", "graph", "links", "agents"].iter().enumerate() {
        if at > 0 {
            text = text.add(Tone::Plain, " ");
        }
        text = if at == on {
            text.add(Tone::Pill, format!(" {} {name} ", at + 1))
        } else {
            text.add(Tone::Dim, format!("{} {name}", at + 1))
        };
    }
    text
}

/// The keys of a view: each key lit, what it does dim.
pub fn keys(pairs: &[(&str, &str)]) -> Text {
    let mut text = Text::new();
    for (key, does) in pairs {
        text = text
            .add(Tone::Bold, *key)
            .add(Tone::Dim, format!(" {does}  "));
    }
    text
}

/// One repository on one line: its mark, name, branch, uncommitted paths
/// and distance from its base. Unknown shows as `?`, never as none.
fn repository(row: &Row, picked: bool, name_width: usize, width: usize) -> Text {
    let pick = if picked {
        Text::of(Tone::Accent, "▸")
    } else {
        Text::of(Tone::Plain, " ")
    };
    let name = format!("{:<name_width$}", cut(&row.name, name_width));
    if !row.present {
        return pick
            .add(Tone::Dim, "  ")
            .add(Tone::Dim, name)
            .add(Tone::Dim, " not here")
            .fit(width);
    }
    let mut left = pick
        .then(proof_mark(row.proof.as_deref()))
        .add(Tone::Plain, " ")
        .add(if picked { Tone::Bold } else { Tone::Plain }, name)
        .add(
            Tone::Accent,
            if row.sessions.is_empty() {
                "  "
            } else {
                " ●"
            },
        )
        .add(Tone::Plain, " ");
    let mut right = Text::new();
    match row.changed {
        Some(0) => {}
        Some(paths) => right = right.add(Tone::Warn, format!("±{paths}")),
        None => right = right.add(Tone::Warn, "±?"),
    }
    let gap = |text: Text| {
        if text.width() > 0 {
            text.add(Tone::Plain, " ")
        } else {
            text
        }
    };
    match (row.ahead, row.behind) {
        (None, None) => right = gap(right).add(Tone::Dim, "↕?"),
        (ahead, behind) => {
            if let Some(ahead) = ahead.filter(|&n| n > 0) {
                right = gap(right).add(Tone::Cool, format!("↑{ahead}"));
            }
            if let Some(behind) = behind.filter(|&n| n > 0) {
                right = gap(right).add(Tone::Dim, format!("↓{behind}"));
            }
        }
    }
    let room = width.saturating_sub(left.width() + right.width() + 1);
    let branch = row.branch.as_deref().unwrap_or("?");
    let tone = if branch == "main" || branch == "master" {
        Tone::Dim
    } else {
        Tone::Cool
    };
    left = left.add(tone, cut(branch, room));
    ends(left, right, width)
}

/// Every repository of the projects shown, a line each.
pub fn repositories(view: &View, picked: Option<&str>, width: usize) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let name_width = view
        .projects
        .iter()
        .flat_map(|project| &project.repositories)
        .map(|row| row.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(13);
    let mut rows = Vec::new();
    let mut marked = false;
    for project in &view.projects {
        if view.projects.len() > 1 {
            rows.push(Text::of(Tone::Bold, cut(&project.name, inner)));
        }
        for row in &project.repositories {
            // A repository in two projects is picked once, where it is first.
            let pick = !marked && picked == Some(row.name.as_str());
            marked |= pick;
            rows.push(repository(row, pick, name_width, inner));
        }
    }
    for warning in &view.warnings {
        rows.extend(wrapped(Tone::Warn, &format!("not read: {warning}"), inner));
    }
    let note = if view.warnings.is_empty() {
        Text::new()
    } else {
        Text::of(Tone::Warn, "partial")
    };
    card("REPOSITORIES", note, rows, width)
}

/// The agent sessions of the last day, at most `limit`, then what each
/// agent wrote. `beat` turns each second draw, for the mark of a session
/// that wrote within the last two minutes.
pub fn agents(
    facts: Option<&Facts>,
    now: u64,
    limit: usize,
    bars: bool,
    beat: bool,
    width: usize,
) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let agents = match facts.map(|facts| &facts.agents) {
        None => {
            return card(
                "AGENTS",
                Text::new(),
                vec![Text::of(Tone::Dim, "asking vessa…")],
                width,
            );
        }
        Some(Err(why)) => {
            return card(
                "AGENTS",
                Text::new(),
                wrapped(Tone::Warn, &format!("not read: {why}"), inner),
                width,
            );
        }
        Some(Ok(agents)) => agents,
    };
    let digest = &agents.digest;
    let live = digest.live.len();
    let note = Text::of(
        if live > 0 { Tone::Good } else { Tone::Dim },
        format!("{live} live"),
    )
    .add(Tone::Dim, format!(" · {} today", digest.sessions.len()));

    let mut sessions: Vec<&tools::Session> = digest.sessions.iter().collect();
    sessions.sort_by(|a, b| b.last_at.cmp(&a.last_at));
    let mut rows = Vec::new();
    // mark(2) agent(8) folder(rest) age(4) tokens(6)
    let folder = inner.saturating_sub(2 + 8 + 1 + 1 + 4 + 1 + 6).max(4);
    for session in sessions.iter().take(limit) {
        let age = tools::age(session.last_at.as_deref(), now);
        let fresh = age == "now";
        let mark = match (fresh, beat) {
            (true, true) => Text::of(Tone::Good, "● "),
            (true, false) => Text::of(Tone::Dim, "● "),
            (false, _) => Text::of(Tone::Plain, "  "),
        };
        rows.push(
            mark.add(Tone::Plain, format!("{:<8} ", cut(&session.agent, 8)))
                .add(
                    Tone::Dim,
                    format!(
                        "{:<folder$} ",
                        cut(&tools::place(session.cwd.as_deref()), folder)
                    ),
                )
                .add(
                    if fresh { Tone::Good } else { Tone::Dim },
                    format!("{age:>4} "),
                )
                .add(
                    Tone::Plain,
                    format!("{:>6}", tools::tokens(session.output_tokens)),
                ),
        );
    }
    if sessions.len() > limit {
        rows.push(Text::of(
            Tone::Dim,
            format!("  … {} more · 4 for all", sessions.len() - limit),
        ));
    }
    if bars {
        rows.extend(written(digest, inner));
    }
    if agents.partial {
        rows.push(Text::of(Tone::Warn, "partial: vessa could not read it all"));
    }
    card("AGENTS", note, rows, width)
}

/// What each agent wrote over the last day, as bars against the most.
fn written(digest: &tools::Digest, width: usize) -> Vec<Text> {
    if digest.by_agent.is_empty() {
        return Vec::new();
    }
    let most = digest
        .by_agent
        .iter()
        .filter_map(|by| by.output_tokens)
        .max()
        .unwrap_or(0);
    let cells = width.saturating_sub(8 + 1 + 1 + 6).max(4);
    let mut rows = vec![Text::of(Tone::Dim, "tokens written, last day")];
    let mut by_agent: Vec<&tools::ByAgent> = digest.by_agent.iter().collect();
    by_agent.sort_by_key(|by| std::cmp::Reverse(by.output_tokens));
    for by in by_agent {
        let label = Text::of(Tone::Plain, format!("{:<8} ", cut(&by.agent, 8)));
        let amount = Text::of(
            Tone::Plain,
            format!(" {:>6}", tools::tokens(by.output_tokens)),
        );
        rows.push(match by.output_tokens {
            Some(count) if most > 0 => label
                .then(bar(count as f64 / most as f64, cells))
                .then(amount),
            // Not recorded is not nothing: no bar, and a `?`.
            _ => label
                .add(Tone::Dim, format!("{:<cells$}", "not recorded"))
                .then(amount),
        });
    }
    rows
}

/// What each agent read and wrote over the last day, for the agents view.
pub fn totals(facts: Option<&Facts>, width: usize) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let Some(Ok(agents)) = facts.map(|facts| &facts.agents) else {
        return Vec::new();
    };
    let mut rows = written(&agents.digest, inner);
    for by in &agents.digest.by_agent {
        let sessions = match by.sessions {
            Some(1) => "1 session".to_string(),
            Some(count) => format!("{count} sessions"),
            None => "? sessions".to_string(),
        };
        rows.push(Text::of(
            Tone::Dim,
            format!(
                "{:<8} {sessions}, read {}",
                cut(&by.agent, 8),
                tools::tokens(by.input_tokens),
            ),
        ));
    }
    card("LAST DAY", Text::new(), rows, width)
}

/// The numbers in ohjaa's summary line, where it reads as expected.
#[derive(Debug, PartialEq, Eq)]
struct Knowledge {
    current: u64,
    waiting: u64,
    errors: u64,
    flags: u64,
    briefing: Option<(u64, u64)>,
}

fn knowledge_from(summary: &str) -> Option<Knowledge> {
    let mut current = None;
    let mut waiting = None;
    let mut errors = None;
    let mut flags = None;
    let mut briefing = None;
    for part in summary.split(", ") {
        let part = part.trim();
        if let Some(sizes) = part.strip_prefix("briefing ") {
            let (used, of) = sizes.split_once('/')?;
            briefing = Some((used.trim().parse().ok()?, of.trim().parse().ok()?));
            continue;
        }
        let (count, what) = part.split_once(' ')?;
        let count: u64 = count.parse().ok()?;
        match what {
            what if what.starts_with("current") => current = Some(count),
            what if what.starts_with("waiting") => waiting = Some(count),
            what if what.starts_with("error") => errors = Some(count),
            what if what.starts_with("flag") => flags = Some(count),
            _ => return None,
        }
    }
    Some(Knowledge {
        current: current?,
        waiting: waiting?,
        errors: errors?,
        flags: flags?,
        briefing,
    })
}

/// What ohjaa says of the knowledge here. A summary that does not read as
/// expected is shown as ohjaa wrote it.
pub fn knowledge(facts: Option<&Facts>, width: usize) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let summary = match facts.map(|facts| &facts.knowledge) {
        None => {
            return card(
                "KNOWLEDGE",
                Text::new(),
                vec![Text::of(Tone::Dim, "asking ohjaa…")],
                width,
            );
        }
        Some(Err(why)) => {
            return card(
                "KNOWLEDGE",
                Text::new(),
                wrapped(Tone::Warn, &format!("not read: {why}"), inner),
                width,
            );
        }
        Some(Ok(summary)) => summary,
    };
    let Some(known) = knowledge_from(summary) else {
        return card(
            "KNOWLEDGE",
            Text::new(),
            wrapped(Tone::Plain, summary, inner),
            width,
        );
    };
    let count = |count: u64, what: &str, loud: Tone| {
        Text::of(if count > 0 { loud } else { Tone::Dim }, count.to_string())
            .add(Tone::Dim, format!(" {what}"))
    };
    let mut rows = vec![
        Text::of(Tone::Bold, known.current.to_string())
            .add(Tone::Dim, " current   ")
            .then(count(known.waiting, "to decide", Tone::Warn)),
        count(known.flags, "flagged", Tone::Warn)
            .add(Tone::Plain, "   ")
            .then(count(known.errors, "errors", Tone::Bad)),
    ];
    if let Some((used, of)) = known.briefing.filter(|(_, of)| *of > 0) {
        let sizes = format!(" {used}/{of}");
        let cells = inner.saturating_sub(9 + sizes.chars().count()).max(4);
        rows.push(
            Text::of(Tone::Dim, "briefing ")
                .then(bar(used as f64 / of as f64, cells))
                .add(Tone::Dim, sizes),
        );
    }
    let note = if known.waiting > 0 {
        Text::of(Tone::Warn, "ohjaa review")
    } else {
        Text::new()
    };
    card("KNOWLEDGE", note, rows, width)
}

/// What is known of a repository's graph when a frame is drawn.
pub enum Branches<'a> {
    Reading,
    Read(&'a Graph),
    NotRead(&'a str),
}

fn graph_marks(proof: &Proof, review: &Review) -> Text {
    let proof = match proof {
        Proof::Passed => Text::of(Tone::Good, "✓"),
        Proof::Failed => Text::of(Tone::Bad, "✗"),
        Proof::NotRun => Text::of(Tone::Warn, "○"),
        Proof::NoGate => Text::of(Tone::Dim, "–"),
        Proof::Other(_) | Proof::Unknown(_) => Text::of(Tone::Warn, "?"),
    };
    let review = match review {
        Review::None => Text::new(),
        Review::Fine => Text::of(Tone::Good, "fine "),
        Review::Look => Text::of(Tone::Warn, "look "),
        Review::Blocking => Text::of(Tone::Bad, "BLOCKING "),
        Review::Overruled => Text::of(Tone::Warn, "overruled "),
        Review::Stale => Text::of(Tone::Dim, "stale "),
        Review::Unreadable => Text::of(Tone::Warn, "review? "),
    };
    review.then(proof)
}

/// The branches in progress of one repository, a line for each tip: its
/// place in the graph, commit, branches, any review, and sykli's mark. At
/// most `most` rows; what does not fit is counted.
pub fn graph(name: &str, branches: Branches<'_>, most: usize, width: usize) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let title = format!("GRAPH {}", cut(name, inner.saturating_sub(8)));
    let graph = match branches {
        Branches::Reading => {
            return card(
                &title,
                Text::new(),
                vec![Text::of(Tone::Dim, "asking git and sykli…")],
                width,
            );
        }
        Branches::NotRead(why) => {
            return card(
                &title,
                Text::new(),
                wrapped(Tone::Warn, &format!("not read: {why}"), inner),
                width,
            );
        }
        Branches::Read(graph) => graph,
    };
    let lanes = |marks: &str, base: bool| {
        let mut text = Text::new();
        for mark in marks.trim_end().chars() {
            text = match mark {
                '*' if base => text.add(Tone::Bold, "◉"),
                '*' => text.add(Tone::Accent, "●"),
                '|' => text.add(Tone::Dim, "│"),
                '/' => text.add(Tone::Dim, "╱"),
                '\\' => text.add(Tone::Dim, "╲"),
                other => text.add(Tone::Dim, other.to_string()),
            };
        }
        text
    };
    let mut rows = Vec::new();
    for line in &graph.lines {
        match line {
            GraphLine::Edge { graph } => rows.push(lanes(graph, false)),
            GraphLine::Tip { graph: marks, tip } => {
                let base = graph
                    .base
                    .as_ref()
                    .is_some_and(|base| tip.branches.contains(base));
                let short: String = tip.commit.chars().take(7).collect();
                let left = lanes(marks, base).add(Tone::Dim, format!(" {short} ")).add(
                    if base { Tone::Bold } else { Tone::Cool },
                    tip.branches.join(", "),
                );
                rows.push(ends(left, graph_marks(&tip.proof, &tip.review), inner));
                for session in &tip.sessions {
                    let intent = session.intent.as_deref().unwrap_or("intent not given");
                    rows.push(Text::of(Tone::Accent, "  ↳ ").add(
                        Tone::Dim,
                        format!("{} {}: {intent}", session.agent, session.liveness),
                    ));
                }
            }
        }
    }
    for warning in &graph.warnings {
        rows.extend(wrapped(Tone::Warn, &format!("not read: {warning}"), inner));
    }
    if rows.is_empty() {
        rows.push(Text::of(Tone::Dim, "nothing in progress"));
    }
    let tips = graph
        .lines
        .iter()
        .filter(|line| matches!(line, GraphLine::Tip { .. }))
        .count();
    if rows.len() > most {
        let hidden = rows.len() - most.saturating_sub(1);
        rows.truncate(most.saturating_sub(1));
        rows.push(Text::of(
            Tone::Dim,
            format!("… {hidden} more lines · 2 for all"),
        ));
    }
    card(
        &title,
        Text::of(Tone::Dim, format!("{tips} tips")),
        rows,
        width,
    )
}

/// The declared relationships between the repositories shown.
pub fn links(view: &View, width: usize) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let mut rows = Vec::new();
    for relation in &view.relationships {
        rows.push(
            Text::of(Tone::Cool, relation.from.as_str())
                .add(Tone::Accent, " → ")
                .add(Tone::Cool, relation.to.as_str()),
        );
        for line in crate::project::wrap(&relation.kind, "  ", inner) {
            rows.push(Text::of(Tone::Dim, line));
        }
        for path in &relation.missing_contract {
            rows.push(Text::of(Tone::Warn, format!("  missing: {path}")));
        }
    }
    if rows.is_empty() {
        rows.push(Text::of(Tone::Dim, "none declared between these"));
    }
    card(
        "LINKS",
        Text::of(Tone::Dim, format!("{} declared", view.relationships.len())),
        rows,
        width,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Tip;
    use crate::project::ProjectView;
    use crate::side::tools::{Agents, ByAgent, Digest, Session};

    fn plain(lines: &[Text]) -> Vec<String> {
        lines.iter().map(Text::plain).collect()
    }

    fn row(name: &str, branch: &str, proof: Option<&str>) -> Row {
        Row {
            name: name.to_string(),
            present: true,
            branch: Some(branch.to_string()),
            changed: Some(0),
            ahead: Some(0),
            behind: Some(0),
            proof: proof.map(str::to_string),
            sessions: Vec::new(),
        }
    }

    fn view(rows: Vec<Row>) -> View {
        View {
            schema: "test",
            folder: "work".to_string(),
            role: None,
            projects: vec![ProjectView {
                name: "One".to_string(),
                role: None,
                repositories: rows,
            }],
            relationships: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn text_is_cut_and_padded_as_characters_and_coloured_after() {
        let text = Text::of(Tone::Good, "✓").add(Tone::Plain, " proven");
        assert_eq!(text.width(), 8);
        assert_eq!(text.clone().fit(4).plain(), "✓ pr");
        assert_eq!(text.clone().fit(10).plain(), "✓ proven  ");
        assert_eq!(text.clone().fit(1).ansi(Palette::Basic), "\x1b[32m✓\x1b[0m");
        assert!(
            text.ansi(Palette::True)
                .starts_with("\x1b[38;2;72;213;130m✓\x1b[0m")
        );
    }

    #[test]
    fn a_card_is_as_wide_as_asked_whatever_is_in_it() {
        let lines = card(
            "AGENTS",
            Text::of(Tone::Dim, "2 live"),
            vec![
                Text::of(Tone::Plain, "short"),
                Text::of(Tone::Plain, "x".repeat(80)),
            ],
            24,
        );
        let plain = plain(&lines);
        assert_eq!(plain[0], "╭─ AGENTS ──── 2 live ─╮");
        assert_eq!(plain[1], "│ short                │");
        assert_eq!(plain[3], "╰──────────────────────╯");
        assert!(
            plain.iter().all(|line| line.chars().count() == 24),
            "{plain:?}"
        );
    }

    #[test]
    fn a_bar_lights_its_share_and_never_hides_a_little() {
        assert_eq!(bar(0.0, 10).0.len(), 1, "all of it unlit");
        let half = bar(0.5, 10);
        assert_eq!(half.width(), 10);
        assert_eq!(
            half.0.iter().filter(|(tone, _)| *tone != Tone::Dim).count(),
            5
        );
        let little = bar(0.001, 10);
        assert_eq!(
            little
                .0
                .iter()
                .filter(|(tone, _)| *tone != Tone::Dim)
                .count(),
            1
        );
        assert_eq!(bar(7.0, 10).0.len(), 10, "more than all is all");
    }

    #[test]
    fn a_repository_is_one_line_and_what_is_unknown_is_a_question_mark() {
        let mut ahead = row("alpha", "feat/a-long-branch-name-here", Some("proven"));
        ahead.changed = Some(3);
        ahead.ahead = Some(2);
        ahead.behind = Some(40);
        let mut unknown = row("beta", "main", None);
        unknown.changed = None;
        unknown.ahead = None;
        unknown.behind = None;
        let mut gone = row("gamma", "main", None);
        gone.present = false;
        let lines = plain(&repositories(
            &view(vec![
                ahead,
                unknown,
                gone,
                row("delta", "main", Some("failed")),
            ]),
            Some("alpha"),
            38,
        ));
        assert_eq!(lines[0], "╭─ REPOSITORIES ─────────────────────╮");
        assert_eq!(lines[1], "│ ▸✓ alpha   feat/a-long-… ±3 ↑2 ↓40 │");
        assert_eq!(lines[2], "│  ? beta    main              ±? ↕? │");
        assert_eq!(lines[3], "│    gamma not here                  │");
        assert_eq!(lines[4], "│  ✗ delta   main                    │");
        assert!(
            lines.iter().all(|line| line.chars().count() == 38),
            "{lines:?}"
        );
    }

    #[test]
    fn a_view_that_could_not_all_be_read_says_so_in_its_card() {
        let mut partial = view(vec![row("alpha", "main", Some("proven"))]);
        partial
            .warnings
            .push("beta: git did not answer".to_string());
        let lines = plain(&repositories(&partial, None, 38));
        assert!(lines[0].contains("partial"), "{lines:?}");
        assert!(
            lines[2].contains("not read: beta: git did not"),
            "{lines:?}"
        );
    }

    fn facts() -> Facts {
        Facts {
            agents: Ok(Agents {
                digest: Digest {
                    live: vec!["claude".to_string()],
                    by_agent: vec![
                        ByAgent {
                            agent: "claude".to_string(),
                            input_tokens: Some(1_200),
                            output_tokens: Some(2_500_000),
                            sessions: Some(2),
                        },
                        ByAgent {
                            agent: "opencode".to_string(),
                            input_tokens: None,
                            output_tokens: None,
                            sessions: Some(1),
                        },
                    ],
                    sessions: vec![
                        Session {
                            agent: "opencode".to_string(),
                            cwd: Some("/work/alpha".to_string()),
                            last_at: Some("2026-01-02T00:00:00Z".to_string()),
                            output_tokens: None,
                            subagents: None,
                        },
                        Session {
                            agent: "claude".to_string(),
                            cwd: Some("/work/beta".to_string()),
                            last_at: Some("2026-01-02T11:59:30Z".to_string()),
                            output_tokens: Some(2_500_000),
                            subagents: None,
                        },
                    ],
                    warnings: Vec::new(),
                },
                partial: true,
            }),
            knowledge: Ok(
                "18 current, 11 waiting for a decision, 0 error(s), 1 flag(s), briefing 7727/8000"
                    .to_string(),
            ),
        }
    }
    /// 2026-01-02T12:00:00Z.
    const NOON: u64 = 1_767_355_200;

    #[test]
    fn agents_show_who_is_live_what_was_written_and_what_is_not_known() {
        let lines = plain(&agents(Some(&facts()), NOON, 4, true, true, 38));
        assert_eq!(lines[0], "╭─ AGENTS ──────── 1 live · 2 today ─╮");
        assert_eq!(lines[1], "│ ● claude   beta         now   2.5M │");
        assert_eq!(lines[2], "│   opencode alpha        12h      ? │");
        assert_eq!(lines[3], "│ tokens written, last day           │");
        assert!(lines[4].starts_with("│ claude   ━━━━"), "{lines:?}");
        assert!(lines[4].ends_with("   2.5M │"), "{lines:?}");
        assert!(lines[5].starts_with("│ opencode not recorded"), "{lines:?}");
        assert!(lines[5].ends_with("      ? │"), "{lines:?}");
        assert_eq!(lines[6], "│ partial: vessa could not read it a │");
        assert!(
            lines.iter().all(|line| line.chars().count() == 38),
            "{lines:?}"
        );
        // One more than the limit is counted, not dropped.
        let few = plain(&agents(Some(&facts()), NOON, 1, true, true, 38));
        assert!(few[2].contains("… 1 more · 4 for all"), "{few:?}");
    }

    #[test]
    fn a_tool_that_said_nothing_is_named_not_shown_as_an_empty_card() {
        let waiting = plain(&agents(None, NOON, 4, true, true, 38));
        assert!(waiting[1].contains("asking vessa…"), "{waiting:?}");
        let absent = Facts {
            agents: Err("vessa is not installed".to_string()),
            knowledge: Err("ohjaa is not installed".to_string()),
        };
        let lines = plain(&agents(Some(&absent), NOON, 4, true, true, 38));
        assert!(
            lines[1].contains("not read: vessa is not installed"),
            "{lines:?}"
        );
        let lines = plain(&knowledge(Some(&absent), 38));
        assert!(
            lines[1].contains("not read: ohjaa is not installed"),
            "{lines:?}"
        );
    }

    #[test]
    fn ohjaas_numbers_are_laid_out_and_an_unexpected_line_is_shown_whole() {
        let lines = plain(&knowledge(Some(&facts()), 38));
        assert_eq!(lines[0], "╭─ KNOWLEDGE ───────── ohjaa review ─╮");
        assert_eq!(lines[1], "│ 18 current   11 to decide          │");
        assert_eq!(lines[2], "│ 1 flagged   0 errors               │");
        assert!(lines[3].starts_with("│ briefing ━━━"), "{lines:?}");
        assert!(lines[3].ends_with(" 7727/8000 │"), "{lines:?}");
        let odd = Facts {
            agents: Err(String::new()),
            knowledge: Ok("all is well".to_string()),
        };
        assert_eq!(
            plain(&knowledge(Some(&odd), 38))[1],
            "│ all is well                        │"
        );
        assert_eq!(knowledge_from("3 current, 1 of something else"), None);
    }

    fn tip(marks: &str, commit: &str, branch: &str, proof: Proof, review: Review) -> GraphLine {
        GraphLine::Tip {
            graph: marks.to_string(),
            tip: Tip {
                commit: commit.to_string(),
                branches: vec![branch.to_string()],
                subject: String::new(),
                proof,
                review,
                sessions: Vec::new(),
            },
        }
    }

    #[test]
    fn a_tip_is_one_line_with_its_mark_at_the_edge_and_the_rest_is_counted() {
        let graph_of = Graph {
            schema: "test",
            base: Some("origin/main".to_string()),
            lines: vec![
                tip(
                    "*",
                    "aaaaaaa1111",
                    "feat/one",
                    Proof::Passed,
                    Review::Blocking,
                ),
                tip(
                    "| *",
                    "bbbbbbb2222",
                    "feat/two",
                    Proof::NotRun,
                    Review::None,
                ),
                GraphLine::Edge {
                    graph: "|/".to_string(),
                },
                tip(
                    "*",
                    "ccccccc3333",
                    "origin/main",
                    Proof::Failed,
                    Review::None,
                ),
            ],
            warnings: Vec::new(),
        };
        let lines = plain(&graph("alpha", Branches::Read(&graph_of), 9, 38));
        assert_eq!(lines[0], "╭─ GRAPH alpha ───────────── 3 tips ─╮");
        assert_eq!(lines[1], "│ ● aaaaaaa feat/one      BLOCKING ✓ │");
        assert_eq!(lines[2], "│ │ ● bbbbbbb feat/two             ○ │");
        assert_eq!(lines[3], "│ │╱                                 │");
        assert_eq!(lines[4], "│ ◉ ccccccc origin/main            ✗ │");
        let short = plain(&graph("alpha", Branches::Read(&graph_of), 2, 38));
        assert_eq!(short.len(), 4, "two rows and the two edges of the card");
        assert_eq!(short[2], "│ … 3 more lines · 2 for all         │");
        let reading = plain(&graph("alpha", Branches::Reading, 9, 38));
        assert!(reading[1].contains("asking git and sykli…"), "{reading:?}");
    }

    #[test]
    fn the_view_on_screen_is_lit_and_the_row_of_views_fits_the_pane() {
        assert_eq!(tabs(0).plain(), " 1 projects  2 graph 3 links 4 agents");
        assert_eq!(tabs(3).plain(), "1 projects 2 graph 3 links  4 agents ");
        for on in 0..4 {
            assert!(tabs(on).width() <= 38);
            assert_eq!(
                tabs(on)
                    .0
                    .iter()
                    .filter(|(tone, _)| *tone == Tone::Pill)
                    .count(),
                1
            );
        }
        assert!(legend().width() <= 38);
        assert_eq!(
            header("SDLC", "12:54 UTC", 38).plain(),
            "▍TOIMIJA  SDLC               12:54 UTC"
        );
    }
}
