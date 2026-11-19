//! Declared connections and bounded, read-only dependency evidence.
use super::NotRead;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const COVERAGE: &str = "Dependency coverage is partial: root Cargo manifests and lockfiles, plus literal Git sources in mix.lock and rebar.lock. No build files are executed; runtime connections and nested manifests need declarations.";
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredRelationship {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Relationship {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub basis: &'static str,
    pub source: String,
}

pub fn collect(
    entries: &[PathBuf],
    declared: &[DeclaredRelationship],
    gaps: &mut Vec<NotRead>,
) -> Vec<Relationship> {
    let repositories: Vec<_> = entries
        .iter()
        .filter(|path| fs::symlink_metadata(path.join(".git")).is_ok())
        .filter_map(|path| {
            Some((
                path.file_name()?.to_str()?.to_owned(),
                path.canonicalize().ok()?,
            ))
        })
        .collect();
    let mut found = BTreeSet::new();
    for relation in declared {
        if [&relation.from, &relation.to, &relation.kind]
            .iter()
            .any(|v| v.trim().is_empty())
        {
            gap(
                gaps,
                super::WORKSPACE_PATH,
                "relationship from, to and kind must be nonblank",
            );
            continue;
        }
        for endpoint in [&relation.from, &relation.to] {
            if !repositories.iter().any(|(name, _)| name == endpoint) {
                gap(
                    gaps,
                    super::WORKSPACE_PATH,
                    &format!("declared repository {endpoint:?} is not present"),
                );
            }
        }
        found.insert(Relationship {
            from: relation.from.clone(),
            to: relation.to.clone(),
            kind: relation.kind.clone(),
            basis: "declared",
            source: super::WORKSPACE_PATH.into(),
        });
    }
    for (name, repository) in &repositories {
        for file in ["Cargo.toml", "Cargo.lock", "mix.lock", "rebar.lock"] {
            let source = format!("{name}/{file}");
            let Some(text) = read(&repository.join(file), &source, gaps) else {
                continue;
            };
            let mut add = |target: String, kind: &str| {
                if target != *name {
                    found.insert(Relationship {
                        from: name.clone(),
                        to: target,
                        kind: kind.into(),
                        basis: "inferred",
                        source: source.clone(),
                    });
                }
            };
            if file == "Cargo.toml" || file == "Cargo.lock" {
                let value: toml::Value = match toml::from_str(&text) {
                    Ok(value) => value,
                    Err(error) => {
                        gap(gaps, &source, &format!("invalid TOML: {}", error.message()));
                        continue;
                    }
                };
                if file == "Cargo.lock" {
                    if let Some(packages) = value.get("package").and_then(toml::Value::as_array) {
                        for package in packages {
                            if let Some(url) = package
                                .get("source")
                                .and_then(toml::Value::as_str)
                                .and_then(|s| s.strip_prefix("git+"))
                            {
                                add(public_git_url(url), "locked dependency (may be transitive)");
                            }
                        }
                    }
                } else {
                    cargo_dependencies(
                        &value,
                        repository,
                        &repositories,
                        &source,
                        None,
                        gaps,
                        &mut add,
                    );
                }
            } else {
                match locked_git_sources(&text, if file == "mix.lock" { '#' } else { '%' }) {
                    Ok(urls) => {
                        for url in urls {
                            add(
                                public_git_url(&url),
                                "locked dependency (may be transitive)",
                            );
                        }
                    }
                    Err(reason) => gap(gaps, &source, reason),
                }
            }
        }
        // A dynamic build definition is not read as though it were a data file.
        for (manifest, lock) in [("mix.exs", "mix.lock"), ("rebar.config", "rebar.lock")] {
            if repository.join(manifest).exists() && !repository.join(lock).exists() {
                gap(
                    gaps,
                    &format!("{name}/{manifest}"),
                    &format!("not executed; no {lock} to read Git dependencies from"),
                );
            }
        }
    }
    found.into_iter().collect()
}

fn gap(gaps: &mut Vec<NotRead>, source: &str, reason: &str) {
    gaps.push(NotRead {
        what: format!("dependencies: {source}"),
        reason: reason.into(),
    });
}

fn read(path: &Path, source: &str, gaps: &mut Vec<NotRead>) -> Option<String> {
    let result = (|| -> std::io::Result<String> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Err(std::io::Error::other("not a regular file; not followed"));
        }
        let mut text = String::new();
        fs::File::open(path)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_string(&mut text)?;
        if text.len() as u64 > MAX_FILE_BYTES {
            return Err(std::io::Error::other("file exceeds the 1 MiB read limit"));
        }
        Ok(text)
    })();
    match result {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            gap(gaps, source, &error.to_string());
            None
        }
    }
}

fn cargo_dependencies(
    value: &toml::Value,
    repository: &Path,
    repositories: &[(String, PathBuf)],
    source: &str,
    candidate: Option<&str>,
    gaps: &mut Vec<NotRead>,
    add: &mut impl FnMut(String, &str),
) {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = value.get(section).and_then(toml::Value::as_table) {
            for (name, dependency) in deps {
                if let Some(url) = dependency.get("git").and_then(toml::Value::as_str) {
                    add(public_git_url(url), candidate.unwrap_or(section));
                }
                if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
                    match repository.join(path).canonicalize() {
                        Ok(path) => {
                            if let Some((name, _)) = repositories
                                .iter()
                                .filter(|(_, root)| path.starts_with(root))
                                .max_by_key(|(_, root)| root.components().count())
                            {
                                add(name.clone(), candidate.unwrap_or(section));
                            } else {
                                gap(
                                    gaps,
                                    source,
                                    &format!(
                                        "path dependency {name:?} is outside the listed repositories"
                                    ),
                                );
                            }
                        }
                        Err(error) => gap(
                            gaps,
                            source,
                            &format!("path dependency {name:?} cannot be resolved: {error}"),
                        ),
                    }
                }
            }
        }
    }
    if let Some(workspace) = value.get("workspace") {
        cargo_dependencies(
            workspace,
            repository,
            repositories,
            source,
            Some("workspace dependency candidate (may be unused)"),
            gaps,
            add,
        );
    }
    if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            cargo_dependencies(
                target,
                repository,
                repositories,
                source,
                candidate,
                gaps,
                add,
            );
        }
    }
    if let Some(patches) = value.get("patch").and_then(toml::Value::as_table) {
        for patch in patches.values() {
            let mut table = toml::map::Map::new();
            table.insert("dependencies".into(), patch.clone());
            cargo_dependencies(
                &toml::Value::Table(table),
                repository,
                repositories,
                source,
                Some("patch candidate (may be unused)"),
                gaps,
                add,
            );
        }
    }
}

/// URL identity only: credentials, revision selectors and fragments are not
/// needed to describe a repository connection and must not enter the packet.
fn public_git_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    if let Some((scheme, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit('@').next().unwrap_or(authority);
        format!("{scheme}://{host}/{path}")
    } else if let Some((_, host_path)) = url.split_once('@') {
        host_path.into()
    } else {
        url.into()
    }
}

/// Only literal `{git, "url", ...}` / `{:git, "url", ...}` lock entries.
/// Tokenizing keeps comments and quoted examples from becoming connections.
/// This is not an Elixir/Erlang evaluator or a claim to parse the whole file.
fn locked_git_sources(text: &str, comment: char) -> Result<Vec<String>, &'static str> {
    #[derive(PartialEq)]
    enum Token {
        Word(String),
        Text(String),
        Mark(char),
    }
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == comment {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
        } else if c == '"' || c == '\'' {
            let mut value = String::new();
            let mut closed = false;
            while let Some(next) = chars.next() {
                if next == c {
                    closed = true;
                    break;
                }
                if next == '\\' {
                    value.push(chars.next().ok_or("unfinished quoted lock value")?);
                } else {
                    value.push(next);
                }
            }
            if !closed {
                return Err("unfinished quoted lock value");
            }
            tokens.push(Token::Text(value));
        } else if c.is_ascii_alphanumeric() || c == '_' {
            let mut word = String::from(c);
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                word.push(chars.next().unwrap());
            }
            tokens.push(Token::Word(word));
        } else if !c.is_whitespace() {
            tokens.push(Token::Mark(c));
        }
    }
    // Keep matching delimiters so a complete URL in a truncated file does
    // not become a claim that the dependency was successfully locked.
    let mut stack = Vec::new();
    let mut ends = vec![0; tokens.len()];
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::Mark(open @ ('{' | '[' | '(')) => stack.push((i, *open)),
            Token::Mark(close @ ('}' | ']' | ')')) => {
                let Some((start, open)) = stack.pop() else {
                    return Err("unmatched closing delimiter in lockfile");
                };
                if !matches!((open, close), ('{', '}') | ('[', ']') | ('(', ')')) {
                    return Err("mismatched delimiters in lockfile");
                }
                ends[start] = i;
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("unfinished container in lockfile");
    }
    let mut urls = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if *token != Token::Mark('{') {
            continue;
        }
        let start = i + 1 + usize::from(tokens.get(i + 1) == Some(&Token::Mark(':')));
        if tokens.get(start) == Some(&Token::Word("git".into())) {
            let mut fields = Vec::new();
            let mut begin = i + 1;
            let mut at = begin;
            while at < ends[i] {
                if ends[at] > at {
                    at = ends[at] + 1;
                } else if tokens[at] == Token::Mark(',') {
                    fields.push(&tokens[begin..at]);
                    begin = at + 1;
                    at += 1;
                } else {
                    at += 1;
                }
            }
            fields.push(&tokens[begin..ends[i]]);
            let expected = if comment == '#' { 4 } else { 3 };
            if fields.len() != expected || fields.iter().any(|field| field.is_empty()) {
                return Err("incomplete or malformed Git tuple in lockfile");
            }
            let [Token::Text(url)] = fields[1] else {
                return Err("Git source is not a literal URL");
            };
            let revision = if comment == '#' {
                matches!(fields[2], [Token::Text(value)] if !value.is_empty())
                    && fields[3].first() == Some(&Token::Mark('['))
                    && fields[3].last() == Some(&Token::Mark(']'))
            } else {
                matches!(fields[2], [Token::Mark('{'), Token::Word(kind), Token::Mark(','), Token::Text(value), Token::Mark('}')]
                    if matches!(kind.as_str(), "ref" | "tag" | "branch") && !value.is_empty())
            };
            if !revision || url.is_empty() {
                return Err("Git tuple lacks a literal revision or valid options");
            }
            if url.contains("#{") {
                return Err("interpolated Git source is not a literal dependency");
            }
            urls.push(url.clone());
        }
    }
    Ok(urls)
}
