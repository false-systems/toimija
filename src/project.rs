//! Where the work stands across a workspace: one row for each repository
//! of its projects, with its branch, how much is uncommitted, how far it is
//! from its base, what sykli's receipts say of it and who is working there.
//!
//! The workspace packet is read from files. This view asks more, so it costs
//! more: one `git status` and one `sykli status` for each repository shown.
//! Nothing is kept between runs; what could not be read is a warning, the
//! view is still printed, and the exit code is 4.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::command::CommandRunner;
use crate::workspace::{self, OpenSession, WorkspacePacket};

pub const SCHEMA: &str = "toimija-project.v1";

/// The name of the group shown when the workspace file declares no project.
pub const EVERY_REPOSITORY: &str = "every repository";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub schema: &'static str,
    pub folder: String,
    pub role: Option<String>,
    pub projects: Vec<ProjectView>,
    /// The declared relationships between repositories that are shown.
    pub relationships: Vec<Relation>,
    /// What could not be read. With any, this is not the whole picture.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectView {
    pub name: String,
    pub role: Option<String>,
    pub repositories: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relation {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub missing_contract: Vec<String>,
}

/// One repository as it stands. `None` is unknown, never zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub name: String,
    /// Whether the folder holds this repository at all.
    pub present: bool,
    pub branch: Option<String>,
    /// Paths with uncommitted work: changed, staged, conflicted or untracked.
    pub changed: Option<usize>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    /// What sykli says of the branch checked out: `proven`, `failed`, `none`…
    pub proof: Option<String>,
    /// The tasks of the gate that failed, where the proof is `failed` and
    /// sykli names them. Not part of the published view.
    #[serde(skip)]
    pub failed: Vec<String>,
    pub sessions: Vec<OpenSession>,
}

/// What one repository's git and sykli say now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asked {
    pub changed: Option<usize>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub proof: Option<String>,
    /// The tasks that failed, asked for only when the proof is `failed`.
    pub failed: Vec<String>,
    pub warnings: Vec<String>,
}

/// Ask git and the installed sykli about the repository at `dir`.
pub fn ask(runner: &dyn CommandRunner, dir: &Path) -> Asked {
    ask_with(runner, dir, "sykli")
}

pub fn ask_with(runner: &dyn CommandRunner, dir: &Path, sykli: &str) -> Asked {
    let mut asked = Asked::default();
    match runner.run(
        dir,
        "git",
        &["--no-optional-locks", "status", "--porcelain=v2", "-z"],
    ) {
        Ok(output) if output.status == 0 => asked.changed = Some(changed_paths(&output.stdout)),
        Ok(output) => asked.warnings.push(format!(
            "git status failed: {}",
            output.stderr_lossy().trim()
        )),
        Err(error) => asked
            .warnings
            .push(format!("git could not be run: {error}")),
    }
    let mut command = std::process::Command::new(sykli);
    command.current_dir(dir).args(["status", "--json"]);
    crate::command::scrub_git_location_env(&mut command);
    match command.output() {
        Ok(output) => match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
            Ok(status) => {
                let current = status["branches"]
                    .as_array()
                    .and_then(|all| all.iter().find(|branch| branch["current"] == true));
                match current {
                    Some(branch) => {
                        asked.proof = branch["proof"].as_str().map(str::to_owned);
                        asked.ahead = branch["ahead"].as_u64();
                        asked.behind = branch["behind"].as_u64();
                        // Which task failed is in the commit's own records:
                        // one more sykli process, only for a failed gate.
                        if let (Some("failed"), Some(commit)) =
                            (asked.proof.as_deref(), branch["commit"].as_str())
                        {
                            asked.failed = crate::graph::said_from(sykli, dir, commit).1.failed;
                        }
                    }
                    // A detached HEAD is on no branch: sykli lists branches.
                    None => asked.proof = None,
                }
            }
            Err(_) => asked.warnings.push("sykli gave no answer".to_string()),
        },
        Err(_) => asked.warnings.push("sykli is not installed".to_string()),
    }
    asked
}

/// The paths `git status --porcelain=v2 -z` lists. A rename's entry is
/// followed by its old path as a field of its own, which is not a second
/// change.
fn changed_paths(porcelain: &[u8]) -> usize {
    let mut count = 0;
    let mut fields = porcelain.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        match field.first() {
            Some(b'2') => {
                count += 1;
                fields.next();
            }
            Some(b'1' | b'u' | b'?') => count += 1,
            _ => {}
        }
    }
    count
}

/// Ask git and sykli about each of `names`, repositories directly inside
/// `root`, all at once: the answers do not depend on each other, and sykli
/// takes a second or more in a repository with many branches.
pub fn ask_many(root: &Path, names: &[String]) -> BTreeMap<String, Asked> {
    std::thread::scope(|scope| {
        let asking: Vec<_> = names
            .iter()
            .map(|name| {
                let dir = root.join(name);
                scope.spawn(move || ask(&crate::command::ProcessCommandRunner, &dir))
            })
            .collect();
        names
            .iter()
            .cloned()
            .zip(asking)
            .map(|(name, answer)| {
                let asked = answer.join().unwrap_or_else(|_| Asked {
                    warnings: vec!["could not be asked".to_string()],
                    ..Asked::default()
                });
                (name, asked)
            })
            .collect()
    })
}

/// The repositories a view of `packet` shows, each once, in the order met:
/// the members of the chosen projects that are in the folder.
pub fn shown(packet: &WorkspacePacket, root: &Path, only: Option<&str>) -> Result<Vec<String>> {
    let mut names = Vec::new();
    build_from(packet, root, only, &mut |name, _| {
        names.push(name.to_string());
        Asked::default()
    })?;
    Ok(names)
}

/// Build the view of the workspace at `root`. `only` keeps one project, by
/// name, whatever its case. Each repository shown is asked once, however
/// many projects it is in.
pub fn build(runner: &dyn CommandRunner, root: &Path, only: Option<&str>) -> Result<View> {
    let packet = workspace::build(runner, root)?;
    let mut answers = ask_many(root, &shown(&packet, root, only)?);
    build_from(&packet, root, only, &mut |name, _| {
        answers.remove(name).unwrap_or_default()
    })
}

pub fn build_from(
    packet: &WorkspacePacket,
    root: &Path,
    only: Option<&str>,
    ask: &mut dyn FnMut(&str, &Path) -> Asked,
) -> Result<View> {
    let mut warnings: Vec<String> = packet
        .not_read
        .iter()
        .map(|gap| format!("{}: {}", gap.what, gap.reason))
        .collect();

    // With no project declared, every repository of the folder is the one.
    let declared: Vec<(String, Option<String>, Vec<String>)> = if packet.projects.is_empty() {
        vec![(
            EVERY_REPOSITORY.to_string(),
            None,
            packet.repositories.iter().map(|r| r.name.clone()).collect(),
        )]
    } else {
        packet
            .projects
            .iter()
            .map(|p| (p.name.clone(), p.role.clone(), p.repositories.clone()))
            .collect()
    };
    let chosen: Vec<_> = match only {
        Some(name) => {
            let found: Vec<_> = declared
                .iter()
                .filter(|(project, ..)| project.eq_ignore_ascii_case(name))
                .cloned()
                .collect();
            if found.is_empty() {
                let known: Vec<_> = declared.iter().map(|(p, ..)| p.as_str()).collect();
                anyhow::bail!(
                    "no project named {name:?} in {}/{}; it declares: {}",
                    root.display(),
                    workspace::WORKSPACE_PATH,
                    known.join(", ")
                );
            }
            found
        }
        None => declared,
    };

    let mut rows = BTreeMap::new();
    let mut projects = Vec::new();
    for (name, role, members) in chosen {
        let mut repositories = Vec::new();
        for member in members {
            if !rows.contains_key(&member) {
                let listed = packet.repositories.iter().find(|r| r.name == member);
                let row = match listed {
                    Some(repository) => {
                        let asked = ask(&member, &root.join(&member));
                        warnings.extend(
                            asked
                                .warnings
                                .iter()
                                .map(|warning| format!("{member}: {warning}")),
                        );
                        Row {
                            name: member.clone(),
                            present: true,
                            branch: repository.branch.clone(),
                            changed: asked.changed,
                            ahead: asked.ahead,
                            behind: asked.behind,
                            proof: asked.proof,
                            failed: asked.failed,
                            sessions: repository.sessions.clone(),
                        }
                    }
                    None => Row {
                        name: member.clone(),
                        present: false,
                        branch: None,
                        changed: None,
                        ahead: None,
                        behind: None,
                        proof: None,
                        failed: Vec::new(),
                        sessions: Vec::new(),
                    },
                };
                rows.insert(member.clone(), row);
            }
            repositories.push(rows[&member].clone());
        }
        projects.push(ProjectView {
            name,
            role,
            repositories,
        });
    }

    let shown: BTreeSet<&str> = rows.keys().map(String::as_str).collect();
    let relationships = packet
        .relationships
        .iter()
        .filter(|relation| relation.basis == "declared")
        .filter(|r| shown.contains(r.from.as_str()) && shown.contains(r.to.as_str()))
        .map(|relation| Relation {
            from: relation.from.clone(),
            to: relation.to.clone(),
            kind: relation.kind.clone(),
            missing_contract: relation
                .contract
                .iter()
                .filter(|file| file.missing)
                .map(|file| file.path.clone())
                .collect(),
        })
        .collect();

    // A gap about a repository of the folder that is not shown here is not
    // a gap in this view: it stays in the workspace packet, where it belongs.
    let elsewhere: Vec<&str> = packet
        .repositories
        .iter()
        .map(|repository| repository.name.as_str())
        .filter(|name| !shown.contains(name))
        .collect();
    warnings.retain(|warning| {
        !elsewhere.iter().any(|name| {
            warning.starts_with(&format!("{name}: ")) || warning.contains(&format!(" {name}/"))
        })
    });

    Ok(View {
        schema: SCHEMA,
        folder: packet.workspace.folder.clone(),
        role: packet.workspace.role.clone(),
        projects,
        relationships,
        warnings,
    })
}

fn safe(value: &str) -> String {
    crate::path_codec::display(Path::new(value))
}

/// `text` over as many lines as it needs, each starting with `indent` and
/// no wider than `width`. A word longer than a line is cut.
pub fn wrap(text: &str, indent: &str, width: usize) -> Vec<String> {
    let room = width.saturating_sub(indent.chars().count()).max(8);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word = fit(word, room);
        let longer = line.chars().count() + 1 + word.chars().count();
        if !line.is_empty() && longer > room {
            lines.push(format!("{indent}{line}"));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(format!("{indent}{line}"));
    }
    lines
}

/// `value` cut to `width` characters, with an ellipsis when it was longer.
fn fit(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let mut cut: String = value.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The view as lines no wider than `width`, for a terminal or a narrow pane.
/// State is said in words as well as marks, so nothing depends on colour.
pub fn render_lines(view: &View, width: usize) -> Vec<String> {
    render_lines_with(view, width, None)
}

/// The mark that ends the line of the repository picked in `toimija side`.
pub const PICKED: char = '◂';

/// As [`render_lines`], with the repository named `picked` marked wherever
/// it is listed.
pub fn render_lines_with(view: &View, width: usize, picked: Option<&str>) -> Vec<String> {
    let width = width.max(24);
    let mut lines = Vec::new();
    for (index, project) in view.projects.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.push(fit(&safe(&project.name), width));
        if let Some(role) = &project.role {
            lines.push(fit(&format!("  {}", safe(role)), width));
        }
        for row in &project.repositories {
            let mark = if row.sessions.is_empty() {
                '○'
            } else {
                '●'
            };
            let mut header = fit(
                &format!("{mark} {}", safe(&row.name)),
                width.saturating_sub(2),
            );
            if picked == Some(row.name.as_str()) {
                header.push(' ');
                header.push(PICKED);
            }
            lines.push(header);
            if !row.present {
                lines.push("    not in this folder".to_string());
                continue;
            }
            let changes = match row.changed {
                Some(0) => "clean".to_string(),
                Some(count) => format!("{count} changed"),
                None => "changes unknown".to_string(),
            };
            let proof = row.proof.as_deref().unwrap_or("proof unknown");
            let state = format!("{changes}  {proof}");
            let branch = safe(row.branch.as_deref().unwrap_or("branch unknown"));
            let room = width
                .saturating_sub(4 + state.chars().count() + 2)
                .clamp(8, 30);
            lines.push(fit(
                &format!("    {:<room$}  {state}", fit(&branch, room)),
                width,
            ));
            let mut away = Vec::new();
            if let Some(ahead) = row.ahead.filter(|count| *count > 0) {
                away.push(format!("{ahead} ahead"));
            }
            if let Some(behind) = row.behind.filter(|count| *count > 0) {
                away.push(format!("{behind} behind"));
            }
            if !away.is_empty() {
                lines.push(fit(&format!("    {} its base", away.join(", ")), width));
            }
            for session in &row.sessions {
                let intent = session.intent.as_deref().unwrap_or("intent not given");
                lines.push(fit(
                    &format!(
                        "    {} {}: {}",
                        safe(&session.agent),
                        session.liveness,
                        safe(intent)
                    ),
                    width,
                ));
            }
        }
    }
    let relations = render_relations(view, width);
    if !relations.is_empty() {
        lines.push(String::new());
        lines.push("relations".to_string());
        lines.extend(relations);
    }
    lines.extend(render_not_read(view, width));
    lines
}

/// The declared relationships between the repositories shown.
pub fn render_relations(view: &View, width: usize) -> Vec<String> {
    let width = width.max(24);
    let mut lines = Vec::new();
    {
        for relation in &view.relationships {
            lines.push(fit(
                &format!("  {} → {}", safe(&relation.from), safe(&relation.to)),
                width,
            ));
            lines.extend(wrap(&safe(&relation.kind), "    ", width));
            for path in &relation.missing_contract {
                lines.push(fit(&format!("    contract missing: {}", safe(path)), width));
            }
        }
    }
    lines
}

fn render_not_read(view: &View, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    if !view.warnings.is_empty() {
        lines.push(String::new());
        lines.push("not read".to_string());
        for warning in &view.warnings {
            lines.push(fit(&format!("  {}", safe(warning)), width));
        }
    }
    lines
}

pub fn render_text(view: &View, width: usize) -> String {
    let mut out = String::new();
    for line in render_lines(view, width) {
        writeln!(out, "{line}").unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_is_one_change_and_its_old_path_is_not_another() {
        let porcelain = b"1 .M N... 100644 100644 100644 a a src/a.rs\0\
            2 R. N... 100644 100644 100644 a a R100 new.rs\x001 old name.rs\0\
            ? notes.txt\0u UU N... 1 1 1 1 a a a both.rs\0";
        assert_eq!(changed_paths(porcelain), 4);
        assert_eq!(changed_paths(b""), 0);
    }

    #[test]
    fn text_wraps_at_words_and_never_runs_past_the_width() {
        let lines = wrap("reads what the checker proved of each commit", "  ", 20);
        assert_eq!(
            lines,
            ["  reads what the", "  checker proved of", "  each commit"]
        );
        assert!(lines.iter().all(|line| line.chars().count() <= 20));
        assert_eq!(wrap("", "  ", 20), Vec::<String>::new());
    }

    #[test]
    fn a_long_value_is_cut_with_an_ellipsis_and_a_short_one_is_left() {
        assert_eq!(fit("docs/local-checks-workflow", 12), "docs/local-…");
        assert_eq!(fit("main", 12), "main");
    }
}
