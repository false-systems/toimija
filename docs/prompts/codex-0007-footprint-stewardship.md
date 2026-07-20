# Toimija — Implement proposal 0007: footprint stewardship

You are Codex, working in the `toimija` Rust crate. Implement **proposal 0007
completely**, as three small PRs in a fixed order: the history journal, the
unified sweep, and the doctor footprint section. The design is
`docs/proposals/0007-footprint-stewardship.md`; this prompt locks its open
questions and adds the implementation-level decisions. Where this prompt locks
a decision, do not relitigate it. Where it points at a source file, read the
actual struct before writing a field name — never guess an identifier.

Work like an estate steward: the grounds must be kept, but nothing of the
record may be lost. Every deletion you write is preceded by a written memory;
if you ever find yourself deleting first and remembering second, stop — you
have inverted decision D1.

This prompt being dispatched constitutes acceptance of proposal 0007: your
first PR flips its status line to `**Status:** ACCEPTED 2026-07-18 (Yair
dispatched implementation).` and resolves §6 as locked below.

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` must be green at **every commit**, not just the last one. The
pre-commit hook mirrors these gates; do not bypass it. Run
`toimija doctor --strict` before claiming any PR complete.

---

## 0. Load context first (do not skip)

If `$TOIMIJA_LIVE` is set you are wrapped by `toimija run`: read that file
first and re-read it after Git-state changes. Otherwise run
`toimija enter --for-diff --budget 4000`. Then read, in order:

- `AGENTS.md` — boundaries and multi-agent coordination rules. You will open
  and claim a tracking issue per PR before writing code (`gh issue create`
  then `--add-assignee @me`); none exist yet for 0007.
- `docs/proposals/0007-footprint-stewardship.md` — the design. §3's four
  pieces map onto your three PRs; §4's D1–D5 are law.
- `src/sessions.rs` — your foundation for PR 1 and 2:
  `DEFAULT_STALE_AFTER_SECS`, `DEFAULT_RETENTION_SECS`, `Liveness`,
  `PruneReport { removed, kept }`, `prune(runner, repo_root,
  stale_after_secs, retention_secs)`, and the **pure** `prunable(liveness,
  marker, pid_alive, now_secs, retention_secs)` with its doc comment — the
  "deletion demands the certain signal" rules (live never touched, stale with
  a live pid never touched, unreadable markers never pruned) are inherited
  law, not suggestions.
- `src/session.rs` — `SESSIONS_SUBDIR`, `SessionMarker` and its
  `#[serde(default)]` additivity discipline, `open()`, `close()` (PR 1 hooks
  the teardown journal here-adjacent, in the caller), `is_valid_session_id()`.
- `src/handoff.rs` — your template for PR 1: append-only JSONL discipline
  (`handoff.jsonl`, never read-modify-write), `read_all`, the versioned
  record shape, and its schema-agreement test. The history record follows
  this pattern exactly.
- `docs/schemas/handoff-record.v1.schema.json` — the normative-schema house
  style you will mirror for `history-record.v1.schema.json`.
- `src/atomic_write.rs` — `temp_path()` builds `<name>.tmp-{pid}-{sequence}`.
  That suffix pattern is the sweep's orphan signature in PR 2; derive the
  match from one shared constant, do not restate the format in two places.
- `src/config.rs` — `RepoConfig` and its `#[serde(default)]` section pattern
  (`ContextSection` with a hand-written `Default` carrying real defaults is
  your template for `StewardshipSection`).
- `src/cli.rs` — `run_sessions`, `run_enter`, `run_run`, `run_doctor`, and
  `run_handoff` are the handler shapes you extend. Every subcommand doc
  comment carries a "Use this when…" line and high-traffic commands carry an
  `after_help` Examples block; `tests/cli_help.rs` enforces both by name.
- `tests/cli_sessions.rs`, `tests/cli_handoff.rs` — integration-test
  templates (marker fabrication, env handshake, refusal paths).
- `.gitignore` — the generated-surface comments; you will extend it.

Note on `.toimija/runs/`: toimija never writes it (kisko does — see
`src/consistency.rs` and `src/watch.rs` references to
`.toimija/runs/kisko.jsonl`). Toimija's job in PR 2 is only retention over
it. Do not grow any other opinion about its contents.

## 1. What you are building

```
toimija history                # newest-first human listing of concluded sessions
toimija history --json         # all records as a JSON array
toimija clean                  # explicit full sweep, prints a summary
toimija clean --dry-run        # what would go, deleting nothing
toimija sessions --prune       # unchanged surface, now journals before deleting
toimija doctor [--strict]      # grows a footprint section (PR 3)
```

Plus two invisible behaviors: `enter` and `run` end their session setup with
a best-effort opportunistic sweep, and `run` teardown appends the session's
history record at close.

## 2. Contracts

### 2.1 The history record — versioned, schema-documented (hard requirement)

`.toimija/history.jsonl` — append-only JSONL, gitignored, one record per
concluded session. New module `src/footprint.rs` owns it.

| Field | Type | Source |
| --- | --- | --- |
| `version` | `u32` (always `1`) | constant |
| `id` | `String` | marker |
| `agent` | `Vec<String>` | marker argv |
| `intent` | `Option<String>` | marker |
| `scope` | `Vec<String>` | marker declared scope |
| `branch` | `Option<String>` | marker |
| `started_at` | `u64` unix secs | marker |
| `last_seen_at` | `Option<u64>` | marker |
| `end` | enum `closed` \| `reaped_stale` | teardown vs sweep |
| `exit_status` | `Option<i32>` | marker (when closed) |
| `handoffs` | `Vec<String>` | `handoff::read_all` — each record's note/next_step, capped at 10 with `handoffs_total: usize` (the render layer's "capped with honest remaining" convention) |
| `recorded_at` | `u64` unix secs | clock, `session.rs` convention |

Ship `docs/schemas/history-record.v1.schema.json` — draft 2020-12, `$id` the
repo-relative path, every property described, `required` exact,
`additionalProperties: false`, evolution rules in the top-level description —
and the same hand-rolled record↔schema agreement test the handoff schema has.
Read the marker's actual field names before writing this table into code; if
marker and table disagree, the marker wins and you adjust the schema.

### 2.2 Configuration

`repo.yaml` grows one optional block; defaults live in code
(`src/footprint.rs` constants, re-exported through the section's `Default`):

```yaml
stewardship:
  auto_sweep: true            # opportunistic sweep on enter/run
  sessions_retention_days: 7  # surfaces the existing DEFAULT_RETENTION_SECS
  runs_retention_days: 14
  history_max_records: 500
```

`sessions_retention_days` must be derived from / feed the existing
`DEFAULT_RETENTION_SECS` — one number, two spellings is a bug.

### 2.3 The registry

`src/footprint.rs` declares the artifact taxonomy as an enum (illegal states
unrepresentable — a class is a variant, not a string):

`Config`, `LivePacket`, `SessionDir`, `RunTranscript`, `SnapshotLease`,
`TempFile`, `HistoryJournal` — each answering, via methods, its owner, its
retention rule, and whether a given path under `.toimija/` belongs to it.
`classify(path) -> Option<ArtifactClass>` is the single judgment doctor's
"unexplainable file" check (PR 3) is built on. Pure, exhaustively unit-tested
with real path shapes from this repo (including `session.json.tmp-17582-…`).

## 3. Locked decisions

Proposal decisions **D1–D5 stand verbatim.** Additionally:

- **D7-1 — §6/Q1 resolved: teardown journals.** `run` teardown appends the
  `end: closed` record at close time; the sweep appends `end: reaped_stale`
  records only for sessions whose id is not already in the journal (one read
  of the journal per sweep pass — it is bounded, scanning it is fine). A
  session never gets two records.
- **D7-2 — §6/Q2 resolved: runs retention 14 days**, config-overridable.
- **D7-3 — journal-then-delete is one function.** The only code path that
  removes a session directory calls a single
  `reap(dir, marker) -> Result<()>` that appends+fsyncs the record, then
  removes. Append failure → directory stays, error propagates (explicit
  `clean`) or is logged and skipped (opportunistic sweep). No caller gets to
  order these two steps itself.
- **D7-4 — opportunistic is silent-but-logged, explicit is loud.** The
  `enter`/`run` sweep never fails, never blocks, never delays the packet: any
  error is one structured `warn` and done. `toimija clean` propagates errors
  and prints an honest summary (`removed N sessions, M transcripts, K temp
  files; journaled J records; kept X`). Same pass, two temperaments — write
  it once, parameterize the error posture.
- **D7-5 — orphan temp files.** A `*.tmp-{pid}-{seq}` file anywhere under
  `.toimija/` is reclaimable when older than one hour (mtime). No journal
  record — temp files are nobody's memory.
- **D7-6 — compaction is dumb.** When the journal exceeds
  `history_max_records`, drop oldest whole lines until at the cap, via the
  existing `atomic_write::replace`. No dedup, no merging, no cleverness.
- **D7-7 — history reads are total.** `toimija history` tolerates unknown
  optional fields (serde default discipline) and skips malformed lines with a
  counted warning — a corrupt line degrades the listing, never kills it.
- **D7-8 — observability.** Existing style, existing crate: events
  `toimija.footprint.swept` (sessions_removed, runs_removed, temps_removed,
  journaled, kept), `toimija.footprint.journaled` (session, end),
  `toimija.history.shown` (records). Errors carry actionable context
  (`with_context` naming the path), matching `error.rs` conventions.
- **D7-9 — no boundary edits.** The boundary sentence "Agent coordination —
  … synchronous reads and writes during an explicitly-invoked command" in
  `.toimija/repo.yaml` + `AGENTS.md` already covers all of this. Touch
  neither file's boundary bullets. `doctor --strict` green throughout.

## 4. Craft rules — the skills, baked in

These are the standing engineering rules for every commit; they compress the
architecture and dev-practice guidance this repo is built on.

- **Dependency direction.** Decisions are pure, effects are adapters:
  `prunable` is the model — retention judgments, `classify`, record
  derivation, and compaction selection are pure functions unit-tested without
  filesystem or clock (`now_secs` is always a parameter). Filesystem walking,
  journal appends, and deletion live behind them. If a test needs a tempdir
  to check a *decision*, the decision is in the wrong layer.
- **Illegal states unrepresentable.** `end` is an enum, not a string. An
  artifact class is an enum, not a path prefix comparison scattered at call
  sites. A record that would be missing required fields does not construct.
- **Errors are data, posture is explicit.** `Result` everywhere; `anyhow`
  context strings name the path and the operation. The best-effort/loud split
  (D7-4) is a parameter, never a swallowed `let _ =` — the only permitted
  silent discard is the pattern `session.rs` already uses for a vanished
  marker, and it gets a comment stating the constraint.
- **Names are contracts.** `classify`/`derive_record`/`select_compaction` are
  pure; `sweep`/`reap`/`append_record` mutate; `read_history` reads. No `get_`
  that writes, no `check_` that deletes.
- **Test the contract, test names as specifications.** House style:
  `stale_retention_ages_from_the_last_heartbeat_when_present`. Every locked
  decision above gets at least one test whose name states it — e.g.
  `reap_appends_the_record_before_removing_the_directory`,
  `failed_journal_append_keeps_the_session_directory`,
  `teardown_journaled_session_is_not_journaled_again_by_the_sweep`,
  `unknown_artifact_path_is_classified_as_none`. Do not test through private
  internals; if refactoring breaks a test, the test was wrong.
- **No new dependencies**, runtime or dev. Serde, anyhow, tracing, and the
  existing test harness are sufficient. Schema agreement stays hand-rolled.
- **Delete code > write code.** PR 2 unifies; it must leave *less* cleanup
  logic than it found, not a second prune beside the first. If `--prune` and
  `clean` share less than one implementation, you built the wrong thing.

## 5. Order of work — three PRs, bottom-up

Per PR: open/claim its issue, branch from a **fresh** `origin/main`
(`git fetch` first), small gated commits, rebase on `origin/main` before
opening, `Closes #<n>`, describe the decisions exercised and the test
evidence. After each merge: `git pull && cargo install --path .` — a stale
binary lies about the code. Check PR state before every push
(`gh pr view <n> -q .state`) — this repo merges fast.

### PR 1 — `feat/0007-history-journal` (memory before deletion)

1. `src/footprint.rs`: the registry enum + `classify` (§2.3), the history
   record + `append_record`/`read_history` (append-only JSONL, handoff.rs
   discipline), `derive_record(marker, handoffs, end, now)` pure. Unit tests
   per §4.
2. The schema + agreement test (§2.1).
3. `reap()` (D7-3) wired into `sessions::prune` — `--prune` now journals
   first. Integration test: prune a fabricated expired-closed session, assert
   the record exists and the dir does not; assert an unwritable journal keeps
   the dir.
4. Teardown journaling (D7-1) in `run`'s close path + dedup-by-id in the
   sweep path. `.gitignore` gains `.toimija/history.jsonl` under the existing
   generated-surfaces comment block.
5. `toimija history` (+ `--json`), registered, help-complete,
   `tests/cli_help.rs` sweeps extended, `tests/cli_history.rs` mirroring
   `cli_handoff.rs`. Flip 0007's status line. README: one short paragraph in
   the sessions story (the journal promise), commands table row.

### PR 2 — `feat/0007-sweep-clean` (one pass, two temperaments)

1. `sweep()` in `src/footprint.rs`: sessions (existing prune logic through
   `reap`), run transcripts past retention (D7-2), orphan temps (D7-5), with
   the posture parameter (D7-4) and `SweepReport` (extend or supersede
   `PruneReport` — one report type at the end, not two).
2. `StewardshipSection` in `src/config.rs` (§2.2) feeding retention values;
   doctor validates ranges (0 is invalid; refuse, don't clamp).
3. `toimija clean` (+ `--dry-run`), registered, help-complete, integration
   tests: dry-run deletes nothing and lists honestly; full run round-trips
   with `history`.
4. Opportunistic hook at the end of `run_enter`/`run_run` session setup,
   gated on `auto_sweep`; test that a sweep failure does not fail `enter`.
5. Journal compaction (D7-6) invoked from the sweep. README: `clean` row +
   stewardship config block documented.

### PR 3 — `feat/0007-doctor-footprint` (the state contract, visible)

1. Doctor footprint section: per-class counts/sizes, past-retention counts,
   unexplainable paths (via `classify`), gitignore coverage of generated
   classes. `--strict` fails on unexplainable files and missing ignore
   entries (D4).
2. `toimija adopt` writes the missing ignore entries (flag-then-fix, same as
   the boundary drift guard). Tests: a planted alien file under `.toimija/`
   fails strict doctor; adopt heals the ignore case; a clean repo stays
   clean.
3. README trust-model section: one paragraph — footprint accounting as a
   checkable claim. Do not touch the verbatim Git-state thesis sentence or
   the `## Git as the starting point` heading — `tests/docs.rs` pins both.

## 6. Forbidden moves

- No background reclamation: no timers, signal handlers, daemons, threads
  that outlive the command. The sweep exists exactly as long as the command
  does.
- No network. No new dependencies of any kind.
- Never delete without the journal append durably written first (temp files
  excepted, D7-5). Never rewrite `history.jsonl` except compaction via
  `atomic_write::replace`.
- Never weaken the inherited prune safety rules: live pid, stale-with-live-pid,
  and unreadable markers stay untouchable. Do not weaken any existing test to
  make yours pass — if an existing test fights you, re-read §3.
- No opinion about run-transcript *contents*; retention only.
- No cross-repo writes; everything resolves under the discovered repo root.
  No boundary-bullet edits (D7-9). No new top-level directories.
- Do not journal live sessions "for completeness" — the journal is for
  concluded sessions only; live narrative is the session dir's job.

## 7. Acceptance — the final walk of the grounds

- [ ] A fabricated expired session pruned by any path (`--prune`, `clean`,
      opportunistic) leaves exactly one v1 history record and no directory;
      an unwritable journal leaves the directory in place. (D1, D7-3)
- [ ] A `run` session that closes cleanly appears in `toimija history` with
      `end: closed` even if no sweep ever runs; it is never double-recorded.
      (D7-1)
- [ ] `.toimija/runs/` entries and orphaned `*.tmp-*` files past retention go;
      younger ones stay; `clean --dry-run` lists all of it and removes none.
- [ ] `stewardship:` config overrides every knob; absent block behaves
      identically to today's defaults; `doctor` rejects zero retentions.
- [ ] Journal at cap: oldest lines drop, newest survive, file stays valid
      JSONL through a crash (atomic replace).
- [ ] `doctor --strict`: alien file under `.toimija/` fails with the path
      named; missing ignore entry fails; `adopt` heals it; this repo passes.
- [ ] Records match `docs/schemas/history-record.v1.schema.json` exactly
      (enforced by test); `history --json` parses; malformed lines degrade
      with a counted warning. (D7-7)
- [ ] `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` ·
      `cargo test` · `toimija doctor --strict` — green at every commit of all
      three PRs; help texts pass the `cli_help.rs` sweeps; README updated
      per PR.

When every box ticks, the estate keeps itself — and remembers.
