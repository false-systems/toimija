//! Documentation guarantees. The Git-state thesis is part of the product, so
//! the README must state it verbatim; and every command and flag the docs
//! show is one `toimija --help` has.

use clap::CommandFactory;

const README: &str = include_str!("../README.md");
const REFERENCE: &str = include_str!("../docs/reference.md");
const FIRST_LOOP: &str = include_str!("../docs/first-loop.md");
const AGENTS: &str = include_str!("../AGENTS.md");

#[test]
fn readme_states_the_git_state_thesis() {
    assert!(
        README.contains(
            "We do not ask the model to understand the repo first. \
             We ask Git what state the repo is in."
        ),
        "README.md must contain the Git-state thesis sentence"
    );
}

#[test]
fn readme_states_git_is_the_substrate() {
    // The guarantee is the substance, verbatim, not a heading.
    assert!(
        README.contains("Git is not a save button here. Git is the substrate."),
        "README.md must keep the Git-substrate statement"
    );
    assert!(
        README.contains(
            "Git gives us the graph of what happened. \
             Toimija turns that graph into operational context for agents."
        ),
        "README.md must keep the graph-to-context sentence"
    );
}

/// Every `toimija <command> --flag` the docs show, in a code span or a code
/// block, is a command and a flag `toimija --help` has. A doc that sends a
/// reader to a command that is gone is worse than no doc.
#[test]
fn every_command_and_flag_the_docs_show_exists() {
    let mut named = 0;
    let mut unknown = Vec::new();
    for (doc, text) in [
        ("README.md", README),
        ("docs/reference.md", REFERENCE),
        ("docs/first-loop.md", FIRST_LOOP),
        ("AGENTS.md", AGENTS),
    ] {
        for fragment in code_fragments(text) {
            for invocation in invocations(&fragment) {
                named += 1;
                if let Err(problem) = check(&invocation) {
                    unknown.push(format!("{doc}: `{}`: {problem}", invocation.join(" ")));
                }
            }
        }
    }
    assert!(named > 20, "the scan found only {named} invocations");
    assert!(
        unknown.is_empty(),
        "the docs show what `toimija --help` does not have:\n{}",
        unknown.join("\n")
    );
}

/// The check above can fail: a removed command, a removed flag, and a flag
/// of another command are each refused.
#[test]
fn the_docs_check_refuses_what_help_does_not_have() {
    let words = |line: &str| invocations(line).remove(0);
    assert!(check(&words("toimija verify --json")).is_err());
    assert!(check(&words("toimija context --identity --json")).is_err());
    assert!(check(&words("toimija intent \"x\" --spec docs/adr/1.md")).is_err());
    assert!(check(&words("toimija enter --identity --json")).is_ok());
    assert!(check(&words("toimija sessions --prune")).is_err());
    assert!(check(&words("toimija run --spec docs/adr/1.md claude")).is_err());
    assert!(check(&words("toimija enter --for-diff --budget 4000")).is_ok());
    // What follows the agent's name is the agent's.
    assert!(check(&words("toimija run --intent \"x y\" claude --model opus")).is_ok());
    assert!(check(&words("toimija -q sessions --json")).is_ok());
    // Where a command starts, and only there.
    assert_eq!(invocations("$ cd demo && toimija verify").len(), 1);
    assert!(invocations("[main e5a73d4] Set up toimija and sykli").is_empty());
    assert!(invocations("- 0cb97f9 chore: toimija config").is_empty());
}

/// Lines of fenced code blocks, and inline code spans elsewhere.
fn code_fragments(text: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            fragments.push(line.to_string());
            continue;
        }
        let mut spans = line.split('`');
        spans.next();
        while let Some(span) = spans.next() {
            fragments.push(span.to_string());
            spans.next();
        }
    }
    fragments
}

/// Each `toimija …` in `fragment` where a command starts (first, after a
/// prompt `$`, or after `&&`, `||`, `|` or `;`), as words: from `toimija` to
/// the end of the command (a comment, a pipe or `&&` ends it). A commit
/// subject or a line of output that mentions toimija is not a command.
fn invocations(fragment: &str) -> Vec<Vec<String>> {
    let words = split_words(fragment);
    let mut found = Vec::new();
    for (at, word) in words.iter().enumerate() {
        if word != "toimija" || at + 1 >= words.len() {
            continue;
        }
        let starts_a_command =
            at == 0 || ["$", "&&", "||", "|", ";"].contains(&words[at - 1].as_str());
        if !starts_a_command {
            continue;
        }
        let command: Vec<String> = words[at..]
            .iter()
            .take_while(|word| !["#", "|", "||", "&&", ";", ">", "2>&1"].contains(&word.as_str()))
            .cloned()
            .collect();
        found.push(command);
    }
    found
}

/// Split shell-like words: quotes group, and are dropped.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Whether `words` (`toimija`, then its arguments) names a command and flags
/// `toimija --help` has.
fn check(words: &[String]) -> Result<(), String> {
    let top = toimija::cli::Cli::command();
    let mut rest = words.iter().skip(1).peekable();
    // Global flags before the command.
    let command = loop {
        let Some(word) = rest.next() else {
            return Ok(());
        };
        if word.starts_with('-') {
            if !["--help", "-h", "--version", "-V"].contains(&word.as_str()) {
                let takes = flag(&top, word).ok_or(format!("no global flag {word}"))?;
                if takes {
                    rest.next();
                }
            }
            continue;
        }
        break word;
    };
    // A placeholder such as `<command>` names no command.
    if !command.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return Ok(());
    }
    let sub = top
        .find_subcommand(command)
        .ok_or(format!("no command `{command}`"))?;
    while let Some(word) = rest.next() {
        if word == "--" {
            break;
        }
        if word.starts_with('-') && word.len() > 1 {
            if ["--help", "-h"].contains(&word.as_str()) {
                continue;
            }
            let takes = flag(sub, word)
                .or_else(|| flag(&top, word))
                .ok_or(format!("`{command}` has no flag {word}"))?;
            if takes && !word.contains('=') {
                rest.next();
            }
        } else if command == "run" {
            // The agent's command begins: what follows is the agent's.
            break;
        }
    }
    Ok(())
}

/// Whether `command` has the flag `word` (`--name`, `--name=value` or `-x`),
/// and if so whether it takes a value.
fn flag(command: &clap::Command, word: &str) -> Option<bool> {
    let takes = |arg: &clap::Arg| arg.get_action().takes_values();
    if let Some(long) = word.strip_prefix("--") {
        let name = long.split_once('=').map_or(long, |(name, _)| name);
        return command
            .get_arguments()
            .find(|arg| arg.get_long() == Some(name))
            .map(takes);
    }
    let short = word.strip_prefix('-')?.chars().next()?;
    command
        .get_arguments()
        .find(|arg| arg.get_short() == Some(short))
        .map(takes)
}
