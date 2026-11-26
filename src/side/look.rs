//! How the pane looks: text in tones, cards, bars, and the four views built
//! from them.
//!
//! A line is cut and padded as plain characters first and only then given
//! its colours, so a colour never counts as width. With `COLORTERM` naming
//! true colour the tones are exact; otherwise they are the terminal's own
//! sixteen.

use crate::graph::{Graph, Line as GraphLine, Proof, Review};
use crate::project::{Row, View};

use std::collections::HashMap;

use super::follow::WorkingCopy;
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
    /// A number that is the point of its tile.
    Big,
    /// A step along a bar, from 0 at its start to 255 at its end.
    Heat(u8),
    /// A letter of the name: where it stands from 0 to 255, and whether the
    /// light that crosses the name when news arrives is on it.
    Title(u8, bool),
    /// A line of the graph, by which column it runs in.
    Lane(u8),
    /// The mark of something live, from 0 at its faintest to 7.
    Pulse(u8),
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

    fn sgr(self, tone: Tone, lit: bool) -> String {
        let colour = match (self, tone) {
            (_, Tone::Plain) => String::new(),
            (_, Tone::Bold) => "1".to_string(),
            (Palette::True, Tone::Dim) => "38;2;112;118;132".to_string(),
            (Palette::True, Tone::Good) => "38;2;72;213;130".to_string(),
            (Palette::True, Tone::Warn) => "38;2;247;190;74".to_string(),
            (Palette::True, Tone::Bad) => "1;38;2;255;92;92".to_string(),
            (Palette::True, Tone::Accent) => "1;38;2;255;40;20".to_string(),
            (Palette::True, Tone::Cool) => "38;2;122;184;255".to_string(),
            (Palette::True, Tone::Pill) => "1;38;2;255;255;255;48;2;214;30;16".to_string(),
            (Palette::True, Tone::Big) => "1;38;2;240;242;248".to_string(),
            (Palette::True, Tone::Heat(at)) => {
                // Red at the start of the bar, amber at its end.
                let at = u32::from(at);
                format!("38;2;255;{};{}", 40 + at * 160 / 255, 20 + at * 50 / 255)
            }
            (Palette::True, Tone::Title(_, true)) => "1;38;2;255;236;220".to_string(),
            (Palette::True, Tone::Title(at, false)) => {
                let at = u32::from(at);
                format!("1;38;2;255;{};{}", 36 + at * 110 / 255, 16 + at * 30 / 255)
            }
            (Palette::True, Tone::Lane(lane)) => match lane % 5 {
                0 => "38;2;255;70;50",
                1 => "38;2;122;184;255",
                2 => "38;2;72;213;130",
                3 => "38;2;247;190;74",
                _ => "38;2;200;140;255",
            }
            .to_string(),
            (Palette::True, Tone::Pulse(level)) => {
                // The same green, from a third of its light to all of it.
                let light = 90 + u32::from(level.min(7)) * 165 / 7;
                format!(
                    "38;2;{};{};{}",
                    72 * light / 255,
                    213 * light / 255,
                    130 * light / 255
                )
            }
            (Palette::Basic, Tone::Dim) => "2".to_string(),
            (Palette::Basic, Tone::Good) => "32".to_string(),
            (Palette::Basic, Tone::Warn) => "33".to_string(),
            (Palette::Basic, Tone::Bad | Tone::Accent | Tone::Title(..)) => "1;31".to_string(),
            (Palette::Basic, Tone::Cool) => "36".to_string(),
            (Palette::Basic, Tone::Pill) => "1;7".to_string(),
            (Palette::Basic, Tone::Big) => "1".to_string(),
            (Palette::Basic, Tone::Heat(_)) => "31".to_string(),
            (Palette::Basic, Tone::Lane(lane)) => match lane % 5 {
                0 => "31",
                1 => "36",
                2 => "32",
                3 => "33",
                _ => "35",
            }
            .to_string(),
            (Palette::Basic, Tone::Pulse(level)) => {
                if level >= 4 { "32" } else { "2;32" }.to_string()
            }
        };
        // A lit piece sits on a panel a shade off the background. Sixteen
        // colours have no such shade, and the pieces carry marks instead.
        let panel = match (self, lit, tone) {
            (Palette::True, true, tone) if tone != Tone::Pill => "48;2;38;41;52",
            _ => "",
        };
        match (colour.is_empty(), panel.is_empty()) {
            (_, true) => colour,
            (true, false) => panel.to_string(),
            (false, false) => format!("{colour};{panel}"),
        }
    }
}

/// A piece of a line: its text, its tone, and whether it sits on a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub tone: Tone,
    pub lit: bool,
    pub text: String,
}

/// What a click on a line means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    /// The line of a repository: a click picks it.
    Repository(String),
    /// The line of a branch tip: a click takes the shell to the working
    /// copy that has the branch checked out.
    Branch(String),
}

/// One line of the pane: pieces of text, each in a tone, and what a click
/// on it means, if anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text(Vec<Span>, Option<Tag>);

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
            self.0.push(Span {
                tone,
                lit: false,
                text,
            });
        }
        self
    }

    pub fn then(mut self, other: Text) -> Self {
        self.0.extend(other.0);
        self
    }

    /// This line with what a click on it means.
    pub fn tagged(mut self, tag: Tag) -> Self {
        self.1 = Some(tag);
        self
    }

    pub fn tag(&self) -> Option<&Tag> {
        self.1.as_ref()
    }

    /// The whole line on a panel, as it stands now.
    pub fn lit(mut self) -> Self {
        for span in &mut self.0 {
            span.lit = true;
        }
        self
    }

    pub fn width(&self) -> usize {
        self.0.iter().map(|span| span.text.chars().count()).sum()
    }

    /// The characters without their tones.
    #[cfg(test)]
    pub fn plain(&self) -> String {
        self.0.iter().map(|span| span.text.as_str()).collect()
    }

    /// Exactly `width` columns: cut where it is longer, padded where shorter.
    pub fn fit(self, width: usize) -> Self {
        let mut left = width;
        let mut fitted = Text(Vec::new(), self.1);
        for span in self.0 {
            if left == 0 {
                break;
            }
            let part: String = span.text.chars().take(left).collect();
            left -= part.chars().count();
            if !part.is_empty() {
                fitted.0.push(Span { text: part, ..span });
            }
        }
        fitted.add(Tone::Plain, " ".repeat(left))
    }

    pub fn ansi(&self, palette: Palette) -> String {
        let mut out = String::new();
        for span in &self.0 {
            let sgr = palette.sgr(span.tone, span.lit);
            if sgr.is_empty() {
                out.push_str(&span.text);
            } else {
                out.push_str(&format!("\x1b[{sgr}m{}\x1b[0m", span.text));
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
        let tag = row.tag().cloned();
        let line = Text::of(Tone::Dim, "│ ")
            .then(row.fit(inner))
            .add(Tone::Dim, " │");
        lines.push(match tag {
            Some(tag) => line.tagged(tag),
            None => line,
        });
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
        .add(Tone::Cool, "⌂")
        .add(Tone::Dim, " worktree")
}

/// The top line: the name, what is shown, a mark that turns while
/// something is being asked, and the time. `sweep` is the letter of the
/// name the light is on, when news has just arrived.
pub fn header(
    title: &str,
    clock: &str,
    turning: Option<char>,
    sweep: Option<usize>,
    width: usize,
) -> Text {
    let mut name = Text::of(Tone::Accent, "▍");
    for (at, letter) in "TOIMIJA".chars().enumerate() {
        let along = (at * 255 / 6) as u8;
        name = name.add(Tone::Title(along, sweep == Some(at)), letter.to_string());
    }
    let mut right = Text::new();
    if let Some(mark) = turning {
        right = right.add(Tone::Accent, format!("{mark} "));
    }
    ends(
        name.add(Tone::Plain, "  ").add(Tone::Bold, title),
        right.add(Tone::Dim, clock),
        width,
    )
}

/// Three numbers in a band under the views: the agent sessions running
/// now, what the agents wrote over the last day, and the knowledge that
/// waits for a person. A number that is not known is `?`, and a sum that
/// leaves an agent out says so.
pub fn tiles(facts: Option<&Facts>, pulse: u8, width: usize) -> Vec<Text> {
    let each = width / 3;
    let last = width - each * 2;
    let agents = facts.and_then(|facts| facts.agents.as_ref().ok());
    let waiting = facts
        .and_then(|facts| facts.knowledge.as_ref().ok())
        .and_then(|known| counts_from(&known.summary))
        .map(|known| known.waiting);
    let asked = facts.is_some();
    let unknown = if asked { "?" } else { "…" };

    let live = agents.map(|agents| agents.digest.live.len());
    let live_text = live.map_or_else(|| unknown.to_string(), |count| count.to_string());
    let live_tone = match live {
        Some(count) if count > 0 => Tone::Pulse(pulse),
        _ => Tone::Dim,
    };

    let known: Vec<u64> = agents
        .map(|agents| {
            agents
                .digest
                .by_agent
                .iter()
                .filter_map(|by| by.output_tokens)
                .collect()
        })
        .unwrap_or_default();
    let left_out =
        agents.is_some_and(|agents| agents.partial || known.len() < agents.digest.by_agent.len());
    let written = match agents {
        Some(_) if !known.is_empty() => tools::tokens(Some(known.iter().sum())),
        _ => unknown.to_string(),
    };

    let waiting_text = waiting.map_or_else(|| unknown.to_string(), |count| count.to_string());
    let waiting_tone = match waiting {
        Some(count) if count > 0 => Tone::Warn,
        Some(_) => Tone::Good,
        None => Tone::Dim,
    };

    let cell = |tone: Tone, text: &str, width: usize| {
        Text::of(Tone::Plain, " ").add(tone, text).fit(width)
    };
    vec![
        cell(live_tone, &format!("● {live_text}"), each)
            .then(cell(Tone::Big, &written, each))
            .then(cell(waiting_tone, &waiting_text, last))
            .lit(),
        cell(Tone::Dim, "live", each)
            .then(cell(
                Tone::Dim,
                if left_out { "written +?" } else { "written" },
                each,
            ))
            .then(cell(Tone::Dim, "to decide", last))
            .lit(),
    ]
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

/// The view whose name is at `column` (from 0) of the row of views.
pub fn tab_at(on: usize, column: usize) -> Option<usize> {
    let mut start = 0;
    for (at, name) in ["projects", "graph", "links", "agents"].iter().enumerate() {
        if at > 0 {
            start += 1;
        }
        let wide = name.chars().count() + 2 + if at == on { 2 } else { 0 };
        if (start..start + wide).contains(&column) {
            return Some(at);
        }
        start += wide;
    }
    None
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
    let line = ends(left, right, width).tagged(Tag::Repository(row.name.clone()));
    if picked { line.lit() } else { line }
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
    // How many of the repositories that are here sykli calls proven.
    let mut here: Vec<&Row> = Vec::new();
    for row in view
        .projects
        .iter()
        .flat_map(|project| &project.repositories)
    {
        if row.present && !here.iter().any(|seen| seen.name == row.name) {
            here.push(row);
        }
    }
    let proven = here
        .iter()
        .filter(|row| row.proof.as_deref() == Some("proven"))
        .count();
    let note = if !view.warnings.is_empty() {
        Text::of(Tone::Warn, "partial")
    } else if here.is_empty() {
        Text::new()
    } else {
        Text::of(
            if proven == here.len() {
                Tone::Good
            } else {
                Tone::Warn
            },
            format!("{proven}/{} proven", here.len()),
        )
    };
    card("REPOSITORIES", note, rows, width)
}

/// The agent sessions of the last day, at most `limit`, then what each
/// agent wrote. `pulse` is how bright the mark of a session that wrote
/// within the last two minutes is in this frame.
pub fn agents(
    facts: Option<&Facts>,
    now: u64,
    limit: usize,
    bars: bool,
    pulse: u8,
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
        let mark = if fresh {
            Text::of(Tone::Pulse(pulse), "● ")
        } else {
            Text::of(Tone::Plain, "  ")
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
struct Counts {
    current: u64,
    waiting: u64,
    errors: u64,
    flags: u64,
    briefing: Option<(u64, u64)>,
}

fn counts_from(summary: &str) -> Option<Counts> {
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
    Some(Counts {
        current: current?,
        waiting: waiting?,
        errors: errors?,
        flags: flags?,
        briefing,
    })
}

/// What ohjaa says of the knowledge here, and with `flags` what it
/// flagged as maybe no longer holding. A summary that does not read as
/// expected is shown as ohjaa wrote it.
pub fn knowledge(facts: Option<&Facts>, flags: bool, width: usize) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let told = match facts.map(|facts| &facts.knowledge) {
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
        Some(Ok(told)) => told,
    };
    let flagged = |mut rows: Vec<Text>| {
        if flags {
            for flag in &told.flags {
                rows.extend(wrapped(Tone::Warn, &format!("flag: {flag}"), inner));
            }
        }
        rows
    };
    let Some(known) = counts_from(&told.summary) else {
        return card(
            "KNOWLEDGE",
            Text::new(),
            flagged(wrapped(Tone::Plain, &told.summary, inner)),
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
    card("KNOWLEDGE", note, flagged(rows), width)
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

/// An agent session vessa read in a working copy on a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnBranch {
    pub branch: String,
    pub agent: String,
    /// How long ago it last wrote, short.
    pub age: String,
    /// What it wrote, in tokens, short.
    pub written: String,
}

/// What the other tools know about the branches of a graph.
pub struct Around<'a> {
    /// The branch the shell beside the pane has checked out.
    pub here: Option<&'a str>,
    /// Shown while the graph is asked for again.
    pub turning: Option<char>,
    /// Commits each branch is ahead of the base, as sykli counts them.
    pub ahead: &'a HashMap<String, u64>,
    /// When each branch last got a commit, in Unix seconds.
    pub committed: &'a HashMap<String, u64>,
    /// The repository's working copies, with the handover left in each.
    pub copies: &'a [WorkingCopy],
    /// The agent sessions of the last day, by the branch they work on.
    pub agents: &'a [OnBranch],
    /// How many rows of the graph are scrolled past. Set back to what
    /// the graph has when it is more than that.
    pub skip: &'a std::cell::Cell<usize>,
    pub pulse: u8,
    /// Unix seconds, for how long ago a handover was written.
    pub now: u64,
}

/// One tip of the graph before it is known how much room there is.
struct TipRows {
    line: Text,
    /// The commit and its subject, shown where there is room.
    subject: Text,
    /// Agents at work and the handover left there: always shown.
    notes: Vec<Text>,
    mine: bool,
}

/// The branches in progress of one repository. Each tip has a line: its
/// place in the graph, its branches, `⌂` where a working copy has it
/// checked out, the commits it is ahead of the base, any review, and
/// sykli's mark. Under it, where there is room, its commit and subject;
/// and always the agent sessions working there and the handover left
/// there. The tip the shell beside the pane has checked out is lit. At
/// most `most` rows: subjects give way first, then tips are counted.
pub fn graph(
    name: &str,
    branches: Branches<'_>,
    around: &Around<'_>,
    most: usize,
    width: usize,
) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let title = format!("GRAPH {}", cut(name, inner.saturating_sub(8)));
    let asking = |mark: Option<char>| match mark {
        Some(mark) => Text::of(Tone::Accent, format!("{mark} ")),
        None => Text::new(),
    };
    let graph = match branches {
        Branches::Reading => {
            return card(
                &title,
                Text::new(),
                vec![asking(around.turning).add(Tone::Dim, "asking git and sykli…")],
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
    // Each line of the graph keeps one colour down its column. Under a
    // tip's own line its mark becomes the line it sits on.
    let lanes = |marks: &str, base: bool, under: bool| {
        let mut text = Text::new();
        for (column, mark) in marks.trim_end().chars().enumerate() {
            let lane = Tone::Lane((column / 2) as u8);
            text = match mark {
                '*' if under => text.add(lane, "│"),
                '*' if base => text.add(Tone::Bold, "◉"),
                '*' => text.add(lane, "●"),
                '|' => text.add(lane, "│"),
                '/' if under => text.add(Tone::Plain, " "),
                '\\' if under => text.add(Tone::Plain, " "),
                '/' => text.add(lane, "╱"),
                '\\' => text.add(lane, "╲"),
                other => text.add(Tone::Dim, other.to_string()),
            };
        }
        text
    };

    let mut edges_and_tips: Vec<Result<TipRows, Text>> = Vec::new();
    for line in &graph.lines {
        match line {
            GraphLine::Edge { graph } => edges_and_tips.push(Err(lanes(graph, false, false))),
            GraphLine::Tip { graph: marks, tip } => {
                let base = graph
                    .base
                    .as_ref()
                    .is_some_and(|base| tip.branches.contains(base));
                let on = |branch: &str| tip.branches.iter().any(|name| name == branch);
                let mine = around.here.is_some_and(on);
                let copies: Vec<&WorkingCopy> = around
                    .copies
                    .iter()
                    .filter(|copy| copy.branch.as_deref().is_some_and(on))
                    .collect();

                let mut right = Text::new();
                if !copies.is_empty() {
                    right = right.add(Tone::Cool, "⌂ ");
                }
                if let Some(ahead) = tip
                    .branches
                    .iter()
                    .find_map(|name| around.ahead.get(name))
                    .filter(|ahead| **ahead > 0 && !base)
                {
                    right = right.add(Tone::Cool, format!("+{ahead} "));
                }
                // How long since the branch last got a commit. A branch
                // left for two weeks is drawn faint.
                let idle = tip
                    .branches
                    .iter()
                    .filter_map(|name| around.committed.get(name))
                    .max()
                    .map(|at| around.now.saturating_sub(*at));
                if let Some(idle) = idle {
                    right = right.add(Tone::Dim, format!("{} ", tools::ago(idle)));
                }
                let faint = idle.is_some_and(|idle| idle > 14 * 86_400);
                let right = right.then(graph_marks(&tip.proof, &tip.review));
                let left = lanes(marks, base, false).add(Tone::Plain, " ").add(
                    if base || mine {
                        Tone::Bold
                    } else if faint {
                        Tone::Dim
                    } else {
                        Tone::Cool
                    },
                    tip.branches.join(", "),
                );
                // A click goes to the branch a working copy has checked out.
                let reached = copies
                    .first()
                    .and_then(|copy| copy.branch.clone())
                    .or_else(|| tip.branches.first().cloned());
                let line = ends(left, right, inner);
                let line = match reached {
                    Some(branch) => line.tagged(Tag::Branch(branch)),
                    None => line,
                };

                let short: String = tip.commit.chars().take(7).collect();
                let below = || lanes(marks, base, true).add(Tone::Plain, " ");
                let subject = below()
                    .add(Tone::Dim, format!("{short} {}", tip.subject))
                    .fit(inner);

                let mut notes = Vec::new();
                for session in &tip.sessions {
                    let intent = session.intent.as_deref().unwrap_or("intent not given");
                    notes.push(below().add(Tone::Accent, "↳ ").add(
                        Tone::Dim,
                        format!("{} {}: {intent}", session.agent, session.liveness),
                    ));
                }
                for agent in around.agents.iter().filter(|agent| on(&agent.branch)) {
                    let fresh = agent.age == "now";
                    notes.push(
                        below()
                            .add(
                                if fresh {
                                    Tone::Pulse(around.pulse)
                                } else {
                                    Tone::Dim
                                },
                                "● ",
                            )
                            .add(Tone::Plain, format!("{} ", agent.agent))
                            .add(
                                if fresh { Tone::Good } else { Tone::Dim },
                                format!("{} ", agent.age),
                            )
                            .add(Tone::Dim, format!("wrote {}", agent.written)),
                    );
                }
                for left in copies.iter().filter_map(|copy| copy.handover.as_ref()) {
                    let said = left
                        .unfinished
                        .as_deref()
                        .or(left.next.as_deref())
                        .unwrap_or("nothing written");
                    let head = format!(
                        "handover {}: ",
                        tools::ago(around.now.saturating_sub(left.written_at))
                    );
                    let prefix = below();
                    let room = inner.saturating_sub(prefix.width() + 2);
                    // Where the shell is, the handover is read in full lines;
                    // elsewhere one line says there is one.
                    let lines = crate::project::wrap(&format!("{head}{said}"), "", room);
                    let keep = if mine { 4 } else { 1 };
                    for (at, text) in lines.iter().take(keep).enumerate() {
                        let cut_short = at + 1 == keep && lines.len() > keep;
                        let text = if cut_short {
                            cut(&format!("{text} …"), room)
                        } else {
                            text.clone()
                        };
                        notes.push(
                            below()
                                .add(Tone::Warn, if at == 0 { "↳ " } else { "  " })
                                .add(Tone::Dim, text),
                        );
                    }
                }
                edges_and_tips.push(Ok(TipRows {
                    line: if mine { line.lit() } else { line },
                    subject,
                    notes,
                    mine,
                }));
            }
        }
    }
    let warnings: Vec<Text> = graph
        .warnings
        .iter()
        .flat_map(|warning| wrapped(Tone::Warn, &format!("not read: {warning}"), inner))
        .collect();

    // Subjects are shown where everything then still fits; otherwise only
    // under the tip the shell has checked out.
    let rows_with = |subjects: bool| -> Vec<Text> {
        let mut rows = Vec::new();
        for item in &edges_and_tips {
            match item {
                Err(edge) => rows.push(edge.clone()),
                Ok(tip) => {
                    rows.push(tip.line.clone());
                    if subjects || tip.mine {
                        rows.push(tip.subject.clone());
                    }
                    rows.extend(tip.notes.iter().cloned());
                }
            }
        }
        rows.extend(warnings.iter().cloned());
        rows
    };
    let mut rows = rows_with(true);
    if rows.len() > most {
        rows = rows_with(false);
    }
    if rows.is_empty() {
        rows.push(Text::of(Tone::Dim, "nothing in progress"));
    }
    let tips = edges_and_tips.iter().filter(|item| item.is_ok()).count();
    // Scrolled rows leave the top, as far as there is more than fits.
    let skip = around.skip.get().min(rows.len().saturating_sub(most));
    around.skip.set(skip);
    if skip > 0 {
        rows.drain(..skip);
        rows[0] = Text::of(
            Tone::Dim,
            match skip {
                1 => "… 1 line above".to_string(),
                skip => format!("… {skip} lines above"),
            },
        );
    }
    if rows.len() > most {
        let hidden = rows.len() - most.saturating_sub(1);
        rows.truncate(most.saturating_sub(1));
        rows.push(Text::of(
            Tone::Dim,
            format!("… {hidden} more lines · scroll, or 2"),
        ));
    }
    card(
        &title,
        asking(around.turning).add(Tone::Dim, format!("{tips} tips")),
        rows,
        width,
    )
}

/// What needs a person now, the most pressing first, in plain sentences:
/// a gate that failed, a review that blocks, knowledge that waits. With
/// nothing to say it says so, so an empty card is never a question.
pub fn needs_you(
    view: Option<&View>,
    graph: Option<(&str, &Graph)>,
    facts: Option<&Facts>,
    width: usize,
) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let mut needs: Vec<(Tone, String)> = Vec::new();
    let mut failed: Vec<&str> = Vec::new();
    for row in view
        .iter()
        .flat_map(|view| &view.projects)
        .flat_map(|project| &project.repositories)
    {
        if row.proof.as_deref() == Some("failed") && !failed.contains(&row.name.as_str()) {
            failed.push(&row.name);
            needs.push((
                Tone::Bad,
                format!(
                    "{}: the gate failed on {}",
                    row.name,
                    row.branch.as_deref().unwrap_or("its branch")
                ),
            ));
        }
    }
    if let Some((name, graph)) = graph {
        for line in &graph.lines {
            let GraphLine::Tip { tip, .. } = line else {
                continue;
            };
            let branch = tip.branches.first().map_or("a commit", String::as_str);
            if tip.review == Review::Blocking {
                needs.push((Tone::Bad, format!("{name}: a review blocks {branch}")));
            }
            if tip.proof == Proof::Failed {
                let said = format!("{name}: the gate failed on {branch}");
                if !needs.iter().any(|(_, need)| *need == said) {
                    needs.push((Tone::Bad, said));
                }
            }
        }
    }
    if let Some(Ok(told)) = facts.map(|facts| &facts.knowledge)
        && let Some(known) = counts_from(&told.summary)
    {
        if known.errors > 0 {
            needs.push((
                Tone::Bad,
                format!("{} errors in the knowledge: run ohjaa check", known.errors),
            ));
        }
        if known.waiting > 0 {
            needs.push((
                Tone::Warn,
                format!(
                    "{} proposals wait for your decision: run ohjaa review",
                    known.waiting
                ),
            ));
        }
        if known.flags > 0 {
            needs.push((
                Tone::Warn,
                format!("ohjaa flagged {} rule as maybe stale: view 4", known.flags),
            ));
        }
    }

    let count = needs.len();
    let mut rows = Vec::new();
    for (tone, need) in needs.iter().take(4) {
        for (at, line) in crate::project::wrap(need, "", inner.saturating_sub(2))
            .into_iter()
            .enumerate()
        {
            rows.push(Text::of(*tone, if at == 0 { "• " } else { "  " }).add(Tone::Plain, line));
        }
    }
    if count > 4 {
        rows.push(Text::of(Tone::Dim, format!("  … {} more", count - 4)));
    }
    if rows.is_empty() {
        rows.push(Text::of(Tone::Good, "✓ ").add(Tone::Dim, "nothing right now"));
    }
    let note = if count > 0 {
        Text::of(Tone::Warn, count.to_string())
    } else {
        Text::new()
    };
    card("NEEDS YOU", note, rows, width)
}

/// Where the shell beside the pane is: the repository, the branch checked
/// out there, and the files changed and not committed, by name. `changed`
/// is `git status --porcelain` line by line; `None` while it is asked.
pub fn you_are_here(
    repository: &str,
    branch: Option<&str>,
    changed: Option<&Result<Vec<String>, String>>,
    width: usize,
) -> Vec<Text> {
    let inner = width.saturating_sub(4);
    let title = format!("HERE {}", cut(repository, 12));
    let note = Text::of(
        Tone::Cool,
        cut(
            branch.unwrap_or("no branch"),
            inner.saturating_sub(title.chars().count() + 3),
        ),
    );
    let rows = match changed {
        None => vec![Text::of(Tone::Dim, "asking git…")],
        Some(Err(why)) => wrapped(Tone::Warn, &format!("not read: {why}"), inner),
        Some(Ok(lines)) if lines.is_empty() => {
            vec![Text::of(Tone::Good, "✓ ").add(Tone::Dim, "everything is committed")]
        }
        Some(Ok(lines)) => {
            let mut rows = vec![Text::of(
                Tone::Warn,
                match lines.len() {
                    1 => "1 file changed, not committed".to_string(),
                    count => format!("{count} files changed, not committed"),
                },
            )];
            for line in lines.iter().take(5) {
                // `XY path`: the two status letters, then the path.
                let (state, path) = line.split_at(line.len().min(2));
                let path = path.trim();
                // The end of a long path says more than its start.
                let room = inner.saturating_sub(3);
                let shown = if path.chars().count() > room {
                    let tail: String = path
                        .chars()
                        .skip(path.chars().count() - room.saturating_sub(1))
                        .collect();
                    format!("…{tail}")
                } else {
                    path.to_string()
                };
                rows.push(Text::of(Tone::Warn, format!("{state:<2} ")).add(Tone::Plain, shown));
            }
            if lines.len() > 5 {
                rows.push(Text::of(
                    Tone::Dim,
                    format!("   … {} more", lines.len() - 5),
                ));
            }
            rows
        }
    };
    card(&title, note, rows, width)
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
            half.0.iter().filter(|span| span.tone != Tone::Dim).count(),
            5
        );
        let little = bar(0.001, 10);
        assert_eq!(
            little
                .0
                .iter()
                .filter(|span| span.tone != Tone::Dim)
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
        assert_eq!(lines[0], "╭─ REPOSITORIES ──────── 1/3 proven ─╮");
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
    fn a_click_on_a_repository_that_is_here_picks_it() {
        let cards = repositories(&view(vec![row("alpha", "main", None)]), None, 38);
        assert_eq!(cards[0].tag(), None);
        assert_eq!(cards[1].tag(), Some(&Tag::Repository("alpha".to_string())));
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
            knowledge: Ok(tools::Knowledge {
                summary:
                    "18 current, 11 waiting for a decision, 0 error(s), 1 flag(s), briefing 7727/8000"
                        .to_string(),
                flags: vec!["one-rule: names a thing that is gone".to_string()],
            }),
        }
    }
    /// 2026-01-02T12:00:00Z.
    const NOON: u64 = 1_767_355_200;

    #[test]
    fn agents_show_who_is_live_what_was_written_and_what_is_not_known() {
        let lines = plain(&agents(Some(&facts()), NOON, 4, true, 7, 38));
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
        let few = plain(&agents(Some(&facts()), NOON, 1, true, 7, 38));
        assert!(few[2].contains("… 1 more · 4 for all"), "{few:?}");
    }

    #[test]
    fn a_tool_that_said_nothing_is_named_not_shown_as_an_empty_card() {
        let waiting = plain(&agents(None, NOON, 4, true, 7, 38));
        assert!(waiting[1].contains("asking vessa…"), "{waiting:?}");
        let absent = Facts {
            agents: Err("vessa is not installed".to_string()),
            knowledge: Err("ohjaa is not installed".to_string()),
        };
        let lines = plain(&agents(Some(&absent), NOON, 4, true, 7, 38));
        assert!(
            lines[1].contains("not read: vessa is not installed"),
            "{lines:?}"
        );
        let lines = plain(&knowledge(Some(&absent), true, 38));
        assert!(
            lines[1].contains("not read: ohjaa is not installed"),
            "{lines:?}"
        );
    }

    #[test]
    fn ohjaas_numbers_are_laid_out_and_an_unexpected_line_is_shown_whole() {
        let lines = plain(&knowledge(Some(&facts()), false, 38));
        assert_eq!(lines[0], "╭─ KNOWLEDGE ───────── ohjaa review ─╮");
        assert_eq!(lines[1], "│ 18 current   11 to decide          │");
        assert_eq!(lines[2], "│ 1 flagged   0 errors               │");
        assert!(lines[3].starts_with("│ briefing ━━━"), "{lines:?}");
        assert!(lines[3].ends_with(" 7727/8000 │"), "{lines:?}");
        let odd = Facts {
            agents: Err(String::new()),
            knowledge: Ok(tools::Knowledge {
                summary: "all is well".to_string(),
                flags: Vec::new(),
            }),
        };
        assert_eq!(
            plain(&knowledge(Some(&odd), false, 38))[1],
            "│ all is well                        │"
        );
        assert_eq!(counts_from("3 current, 1 of something else"), None);
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

    fn around<'a>(
        ahead: &'a HashMap<String, u64>,
        committed: &'a HashMap<String, u64>,
        copies: &'a [WorkingCopy],
        agents: &'a [OnBranch],
        here: Option<&'a str>,
    ) -> Around<'a> {
        Around {
            here,
            turning: None,
            ahead,
            committed,
            copies,
            agents,
            skip: Box::leak(Box::new(std::cell::Cell::new(0))),
            pulse: 7,
            now: 7_300,
        }
    }

    fn three_tips() -> Graph {
        let mut two = tip(
            "| *",
            "bbbbbbb2222",
            "feat/two",
            Proof::NotRun,
            Review::None,
        );
        if let GraphLine::Tip { tip, .. } = &mut two {
            tip.subject = "second thing".to_string();
        }
        Graph {
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
                two,
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
        }
    }

    #[test]
    fn a_tip_says_what_every_tool_knows_of_its_branch() {
        let graph_of = three_tips();
        let ahead = HashMap::from([("feat/two".to_string(), 4), ("feat/one".to_string(), 0)]);
        // `now` is 7300: one branch got a commit an hour ago, one two hours ago.
        let committed = HashMap::from([
            ("feat/two".to_string(), 3_700u64),
            ("feat/one".to_string(), 0),
        ]);
        let copies = [WorkingCopy {
            dir: "/work/two".into(),
            branch: Some("feat/two".to_string()),
            handover: Some(crate::side::follow::Left {
                written_at: 100,
                unfinished: Some(
                    "the gate is red on the last commit and nobody looked".to_string(),
                ),
                next: None,
            }),
        }];
        let agents = [OnBranch {
            branch: "feat/two".to_string(),
            agent: "claude".to_string(),
            age: "now".to_string(),
            written: "2.5M".to_string(),
        }];
        let cards = graph(
            "alpha",
            Branches::Read(&graph_of),
            &around(&ahead, &committed, &copies, &agents, Some("feat/two")),
            20,
            38,
        );
        let lines = plain(&cards);
        assert_eq!(
            lines,
            [
                "╭─ GRAPH alpha ───────────── 3 tips ─╮",
                "│ ● feat/one           2h BLOCKING ✓ │",
                "│ │ aaaaaaa                          │",
                "│ │ ● feat/two             ⌂ +4 1h ○ │",
                "│ │ │ bbbbbbb second thing           │",
                "│ │ │ ● claude now wrote 2.5M        │",
                "│ │ │ ↳ handover 2h: the gate is red │",
                "│ │ │   on the last commit and       │",
                "│ │ │   nobody looked                │",
                "│ │╱                                 │",
                "│ ◉ origin/main                    ✗ │",
                "│ │ ccccccc                          │",
                "╰────────────────────────────────────╯",
            ]
        );
        // A click on a tip goes to its branch; the lines under it are not
        // places to click.
        let tags: Vec<Option<&Tag>> = cards.iter().map(Text::tag).collect();
        assert_eq!(tags[1], Some(&Tag::Branch("feat/one".to_string())));
        assert_eq!(tags[3], Some(&Tag::Branch("feat/two".to_string())));
        assert_eq!(tags[2], None);
        assert_eq!(tags[5], None);
        // The branch the shell has checked out is lit, and only that one.
        let lit: Vec<usize> = cards
            .iter()
            .enumerate()
            .filter(|(_, line)| line.0.iter().any(|span| span.lit))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(lit, [3]);
    }

    #[test]
    fn with_little_room_subjects_give_way_first_and_then_tips_are_counted() {
        let graph_of = three_tips();
        let (ahead, copies, agents) = (HashMap::new(), [], []);
        let fits = plain(&graph(
            "alpha",
            Branches::Read(&graph_of),
            &around(&ahead, &ahead, &copies, &agents, None),
            4,
            38,
        ));
        assert_eq!(
            fits,
            [
                "╭─ GRAPH alpha ───────────── 3 tips ─╮",
                "│ ● feat/one              BLOCKING ✓ │",
                "│ │ ● feat/two                     ○ │",
                "│ │╱                                 │",
                "│ ◉ origin/main                    ✗ │",
                "╰────────────────────────────────────╯",
            ]
        );
        let short = plain(&graph(
            "alpha",
            Branches::Read(&graph_of),
            &around(&ahead, &ahead, &copies, &agents, None),
            2,
            38,
        ));
        assert_eq!(short.len(), 4, "two rows and the two edges of the card");
        assert_eq!(short[2], "│ … 3 more lines · scroll, or 2      │");
        // Scrolling leaves rows at the top, never past the last that fits.
        let scrolled = around(&ahead, &ahead, &copies, &agents, None);
        scrolled.skip.set(99);
        let lines = plain(&graph("alpha", Branches::Read(&graph_of), &scrolled, 2, 38));
        assert_eq!(scrolled.skip.get(), 2, "four rows, two fit");
        assert_eq!(lines[1], "│ … 2 lines above                    │");
        assert_eq!(lines[2], "│ ◉ origin/main                    ✗ │");
        let mut busy = around(&ahead, &ahead, &copies, &agents, None);
        busy.turning = Some('⠋');
        let reading = plain(&graph("alpha", Branches::Reading, &busy, 9, 38));
        assert!(
            reading[1].contains("⠋ asking git and sykli…"),
            "{reading:?}"
        );
    }

    #[test]
    fn what_needs_a_person_is_said_in_sentences_the_worst_first() {
        let mut broken = view(vec![
            row("alpha", "feat/x", Some("failed")),
            row("beta", "main", Some("proven")),
        ]);
        broken.projects.push(broken.projects[0].clone());
        let graph_of = three_tips();
        let lines = plain(&needs_you(
            Some(&broken),
            Some(("alpha", &graph_of)),
            Some(&facts()),
            38,
        ));
        assert_eq!(
            lines,
            [
                "╭─ NEEDS YOU ──────────────────── 5 ─╮",
                "│ • alpha: the gate failed on feat/x │",
                "│ • alpha: a review blocks feat/one  │",
                "│ • alpha: the gate failed on        │",
                "│   origin/main                      │",
                "│ • 11 proposals wait for your       │",
                "│   decision: run ohjaa review       │",
                "│   … 1 more                         │",
                "╰────────────────────────────────────╯",
            ]
        );
        // Quiet is said, and so is not confused with not asked yet.
        let quiet = plain(&needs_you(
            Some(&view(vec![row("beta", "main", Some("proven"))])),
            None,
            None,
            38,
        ));
        assert_eq!(quiet[0], "╭─ NEEDS YOU ────────────────────────╮");
        assert_eq!(quiet[1], "│ ✓ nothing right now                │");
    }

    #[test]
    fn where_the_shell_is_names_the_files_not_committed() {
        let changed = Ok(vec![
            " M src/side.rs".to_string(),
            "?? a/very/long/path/to/some/deeply/nested/file.txt".to_string(),
        ]);
        let lines = plain(&you_are_here("alpha", Some("feat/two"), Some(&changed), 38));
        assert_eq!(
            lines,
            [
                "╭─ HERE alpha ──────────── feat/two ─╮",
                "│ 2 files changed, not committed     │",
                "│  M src/side.rs                     │",
                "│ ?? …to/some/deeply/nested/file.txt │",
                "╰────────────────────────────────────╯",
            ]
        );
        let clean = plain(&you_are_here("alpha", None, Some(&Ok(Vec::new())), 38));
        assert!(clean[0].contains("no branch"), "{clean:?}");
        assert_eq!(clean[1], "│ ✓ everything is committed          │");
        let asking = plain(&you_are_here("alpha", Some("main"), None, 38));
        assert!(asking[1].contains("asking git…"), "{asking:?}");
        let failed = Err("git did not answer".to_string());
        let lines = plain(&you_are_here("alpha", Some("main"), Some(&failed), 38));
        assert!(
            lines[1].contains("not read: git did not answer"),
            "{lines:?}"
        );
    }

    #[test]
    fn the_band_of_numbers_says_what_is_left_out_and_what_is_not_known_yet() {
        let lines = tiles(Some(&facts()), 7, 38);
        assert_eq!(
            plain(&lines),
            [
                " ● 1         2.5M        11           ",
                " live        written +?  to decide    ",
            ]
        );
        assert!(lines.iter().all(|line| line.width() == 38));
        assert!(
            lines[0].0.iter().all(|span| span.lit),
            "the band is a panel"
        );
        // Before the tools answered nothing is a number, and a tool that
        // could not be asked leaves a question mark, never a nought.
        assert_eq!(
            tiles(None, 0, 38)[0].plain(),
            " ● …         …           …            "
        );
        let absent = Facts {
            agents: Err("vessa is not installed".to_string()),
            knowledge: Err("ohjaa is not installed".to_string()),
        };
        assert_eq!(
            tiles(Some(&absent), 0, 38)[0].plain(),
            " ● ?         ?           ?            "
        );
    }

    #[test]
    fn the_name_carries_the_mark_that_turns_and_the_light_that_crosses_it() {
        let still = header("SDLC", "12:54 UTC", None, None, 38);
        assert!(
            !still
                .0
                .iter()
                .any(|span| matches!(span.tone, Tone::Title(_, true)))
        );
        let busy = header("SDLC", "12:54 UTC", Some('⠙'), Some(3), 38);
        assert_eq!(busy.plain(), "▍TOIMIJA  SDLC             ⠙ 12:54 UTC");
        let lit: Vec<&str> = busy
            .0
            .iter()
            .filter(|span| matches!(span.tone, Tone::Title(_, true)))
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(lit, ["M"], "the fourth letter");
        // Colour never changes what is written.
        assert_eq!(
            Text::of(Tone::Good, "ab").lit().ansi(Palette::True),
            "\x1b[38;2;72;213;130;48;2;38;41;52mab\x1b[0m"
        );
        assert_eq!(
            Text::of(Tone::Good, "ab").lit().ansi(Palette::Basic),
            "\x1b[32mab\x1b[0m"
        );
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
                    .filter(|span| span.tone == Tone::Pill)
                    .count(),
                1
            );
        }
        assert!(legend().width() <= 38);
        // A click on the row of views lands on the view under it.
        assert_eq!(tab_at(0, 0), Some(0));
        assert_eq!(tab_at(0, 11), Some(0));
        assert_eq!(tab_at(0, 12), None, "the gap between two views");
        assert_eq!(tab_at(0, 13), Some(1));
        assert_eq!(tab_at(3, 0), Some(0));
        assert_eq!(tab_at(3, 36), Some(3));
        assert_eq!(tab_at(3, 60), None);
        assert_eq!(
            header("SDLC", "12:54 UTC", None, None, 38).plain(),
            "▍TOIMIJA  SDLC               12:54 UTC"
        );
    }
}
