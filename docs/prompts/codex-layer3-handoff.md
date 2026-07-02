# Toimija — Implement Layer 3: `toimija handoff` (issue #77)

You are Codex, working in the `toimija` Rust crate. Implement **one feature,
completely and precisely**: the session handoff record — the last unbuilt layer
of `docs/proposals/0001-sessions-awareness-handoff.md` (§6, decision D4).

Work like a Bernese clockmaker: measure twice, seat once. Every wheel in this
movement is specified below; your craft is in the fit, not in inventing new
wheels. Where this prompt locks a decision, do not relitigate it. Where it
points you at a source file, read the actual struct before writing a field
name — never guess an identifier.

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` must be green at **every commit**, not just the last one. The
pre-commit hook mirrors these gates; do not bypass it.

---

## 0. Load context first (do not skip)

If `$TOIMIJA_LIVE` is set you are wrapped by `toimija run`: read that file
first and re-read it after Git-state changes. Otherwise run
`toimija enter --for-diff --budget 4000`. Then read, in order:

- `AGENTS.md` — boundaries and the multi-agent coordination rules. You will
  claim issue #77 before writing code.
- `docs/proposals/0001-sessions-awareness-handoff.md` §6, §8, §9/D4 — the
  design you are implementing. §8's hard boundaries are law.
- `src/session.rs` — your foundation. `SessionMarker` (the on-disk record and
  its `#[serde(default)]` additivity discipline), `ambient_session()` (resolves
  `TOIMIJA_SESSION` to a marker **under this repo root only** — a foreign
  handshake can never steer writes into the wrong repo), `is_valid_session_id()`
  (the traversal guard), `declare_intent()` (your template: validate id → read
  marker → refuse closed → mutate → stamp `last_seen_at` → write), `touch()`.
- `src/sessions.rs` — `SessionView`, `report()`, `render_text()`, `liveness()`.
  You will extend the listing with one line.
- `src/context.rs` — `ContextPacket` (fields: `git`, `changed_surface`,
  `validation_plan`, `relevance`, `consistency`) and
  `build_context_packet(runner, repo_root, for_diff)`. The handoff record is
  derived from one packet build — you never re-derive Git state by hand.
- `src/git.rs` — `GitState`: read the actual field names for branch, HEAD,
  merge-base, and the staged/unstaged/untracked/conflict lists before using
  them.
- `src/validate.rs` — `ValidationPlan`: read its tier fields. The handoff
  stores *rendered command strings*; Toimija never runs validation.
- `src/relevance.rs` — `CouplingHint { changed, missing, reason, .. }` and
  `RiskHint { path, reason, action, .. }`.
- `src/cli.rs` — `run_intent` is the shape of your new command handler:
  discover repo root → read `TOIMIJA_SESSION` with a guidance error → call the
  session-layer function → print. Note every subcommand doc comment carries a
  "Use this when…" line and high-traffic commands carry an `after_help`
  Examples block; `tests/cli_help.rs` enforces both by name.
- `tests/cli_intent.rs` — your integration-test template (env handshake,
  closed-session refusal, traversal-id refusal, no-env guidance).

## 1. What you are building

```
toimija handoff [NOTE]              # wrapped agent appends a record for its own session
toimija handoff --json [NOTE]       # same; echo the written record as JSON to stdout
toimija handoff --show <SESSION_ID> # print a session's records (works on closed sessions)
toimija handoff --show <ID> --json  # all records as a JSON array
```

A **handoff record** is a self-contained snapshot a closing (or pausing)
session leaves so a successor agent resumes without re-deriving everything.
It is written **only** by this explicit command (D4 — never automatically on
exit; the existing one-line exit summary in `run` stays and is not a handoff).

Storage: `.toimija/sessions/<id>/handoff.jsonl` — **append-only JSONL**, one
record per line. Multiple handoffs per session are legal and ordered.

## 2. The record contract — versioned, schema-documented (hard requirement)

The record is a public, machine-consumed contract. It ships with a normative
JSON Schema and an explicit version:

- Every record embeds `"version": 1`.
- Create `docs/schemas/handoff-record.v1.schema.json` — JSON Schema
  **draft 2020-12**, `$id` set to the repo-relative path, every property
  described, `required` listing exactly the always-present fields,
  `additionalProperties: false`.
- Evolution rules (state them in the schema's top-level `description`):
  new **optional** fields are additive within v1; any breaking change is a new
  `handoff-record.v2.schema.json` plus a version bump. Readers must tolerate
  unknown optional fields (mirror `SessionMarker`'s `#[serde(default)]`
  discipline).
- A test (`docs.rs` or the new integration file) parses the schema JSON and
  asserts the emitted record's field set matches the schema's `properties` and
  `required` — hand-rolled comparison, **no new dependencies**.

`HandoffRecord` (new module `src/handoff.rs`), serialized with `serde`:

| Field | Type | Source |
| --- | --- | --- |
| `version` | `u32` (always `1`) | constant |
| `session_id` | `String` | marker |
| `written_at` | `u64` unix secs | clock (same convention as `session.rs`) |
| `agent` | `Vec<String>` | marker argv |
| `intent` | `Option<String>` | marker |
| `branch` | `Option<String>` | packet `git` (live at handoff) |
| `entry_head` | `Option<String>` | marker `head` (HEAD at first refresh) |
| `head` | `Option<String>` | packet `git` (live at handoff) |
| `merge_base` | `Option<String>` | packet `git` |
| `staged` / `unstaged` / `untracked` / `conflicts` | `Vec<PathBuf>` each | packet `git` working-tree lists |
| `branch_scope` | `Vec<PathBuf>` | packet `changed_surface` |
| `suggested_validation` | `Vec<String>` | packet `validation_plan`: the focused tier when non-empty, else the fast tier — rendered command strings, **never executed** |
| `coupling` | `Vec<String>` | packet `relevance.coupling`, one line per hint (format from its fields; mirror the packet's own phrasing) |
| `risk` | `Vec<String>` | packet `relevance.risk`, same treatment |
| `consistency_token` | `String` | packet `consistency` token — a successor can check whether the repo moved since the handoff |
| `next_step` | `Option<String>` | the NOTE argument |

List caps: each `Vec` field is capped at 25 entries with a sibling
`<name>_total: usize` carrying the honest full count (mirror the render
layer's "capped with honest remaining" convention). Caps and totals are part
of the schema.

## 3. Locked design decisions

- **D-H1 — explicit only.** No hook into `run`'s exit path. D4 stands.
- **D-H2 — ambient session, active only.** Writing resolves the session
  exactly like `intent`: `TOIMIJA_SESSION` env → `is_valid_session_id` →
  marker under *this* repo root → refuse when closed, with the same style of
  guidance errors. Writing a handoff is evidence of life: stamp
  `last_seen_at` (reuse the `declare_intent` pattern).
- **D-H3 — append, never rewrite.** Open `handoff.jsonl` in append mode; one
  `serde_json` line per record. Never read-modify-write the file.
- **D-H4 — `--show` reads anything, writes nothing.** It accepts a session id
  argument (validated), works on closed sessions, and is read-only. Human
  output prints the **latest** record, clearly labeled with `written_at` age
  and record count; `--json` emits **all** records as an array. Missing file →
  exit non-zero with `no handoff recorded for session <id>` on stderr.
- **D-H5 — one packet build.** All repo-derived fields come from a single
  `build_context_packet(&runner, &repo_root, true)` call at write time.
- **D-H6 — awareness surfaces it.** `toimija sessions` gains one line per
  session when records exist: `handoff: <n> record(s)` (count = lines in the
  file; malformed/missing file → line omitted, never an error). Extend
  `SessionView` with a `handoffs: usize` field (serde-serialized, so
  `--json` carries it too).
- **D-H7 — no boundary edits.** The boundary sentence already covers
  "session handoff records" verbatim in `.toimija/repo.yaml` + `AGENTS.md`.
  Touch neither. `toimija doctor --strict` must stay green — run it before
  claiming completion.
- **D-H8 — observability.** Structured events in the existing style:
  `toimija.handoff.written` (session, records_total) and
  `toimija.handoff.shown` (session, records). Errors follow the existing
  actionable-context conventions.

## 4. Command surface details

- Clap: `Handoff(HandoffArgs)` with a doc comment carrying a first-line
  summary, a "Use this when…" paragraph (distinguish from `intent`: intent
  says *why I am here*, handoff says *what I leave behind*), and an
  `after_help` Examples block:
  ```
  toimija handoff "auth migration half done; run cargo test -p auth next"
  toimija handoff --show 1782951227-52574
  ```
- `HandoffArgs`: optional positional `note: Option<String>`; `--show <ID>`
  conflicts with the positional note; `--json` composes with both modes.
- Register in `Command::name()` as `"handoff"` and in the dispatch match with
  `.map(|()| 0)` — study how existing arms map into the exit-code contract.

## 5. Order of work — one wheel at a time

Claim first: `gh issue edit 77 --add-assignee @me`. Branch `feat/77-handoff`
from a **fresh** `origin/main` (`git fetch` first). Then five commits, each
gated (fmt + clippy + test):

1. **The record and its writer** — `src/handoff.rs`: `HandoffRecord`,
   `write(repo_root, id, note) -> Result<HandoffRecord>` (validate → read
   marker → refuse closed → build packet → derive → append → heartbeat),
   `read_all(repo_root, id) -> Result<Vec<HandoffRecord>>`. Unit tests: record
   derivation caps lists at 25 with honest totals; closed-session refusal;
   traversal-id refusal; append yields ordered records; unknown optional JSON
   fields are tolerated on read.
2. **The schema** — `docs/schemas/handoff-record.v1.schema.json` + the
   record↔schema agreement test. The schema is normative: if a field you built
   in commit 1 is missing here, fix the schema *and* re-check the design
   table above — one of them is wrong.
3. **The CLI** — `handoff` command (write + `--show` paths), registered and
   help-complete. Integration tests in `tests/cli_handoff.rs` (mirror
   `cli_intent.rs`): write-then-show round-trip via env handshake; `--json`
   array output parses; no-env guidance; closed refusal; missing-file `--show`
   exits non-zero. Extend the two hardcoded lists in `tests/cli_help.rs`
   (subcommand sweep + Examples sweep) with `handoff`.
4. **Awareness** — `SessionView.handoffs` + the `handoff: <n> record(s)`
   listing line, with a unit or integration test proving both text and
   `--json` carry it, and that a session without records shows nothing.
5. **Docs** — README: extend the Multi-agent sessions section (the handoff
   story + the schema/versioning promise in one short paragraph), the
   commands table, and the workflow snippet. Do not touch the verbatim
   Git-state thesis sentence or the `## Git as the starting point` heading —
   `tests/docs.rs` pins both.

Then: rebase on `origin/main`, open the PR (`Closes #77`, describe decisions
D-H1…D-H8 and the test evidence), merge only when every gate is green, and
finish with `git pull && cargo install --path .`.

## 6. Forbidden moves (the escapement — do not touch)

- No automatic handoff on `run` exit, no signal handlers, no daemon, no timer.
- No network. No new runtime **or** dev dependencies — the schema test is
  hand-rolled.
- No validation execution: `suggested_validation` is text.
- No editing `.toimija/repo.yaml` boundaries, `AGENTS.md` boundary bullets, or
  the proposals. No new top-level directories beyond `docs/schemas/`.
- No rewriting `handoff.jsonl` — append only, even on retry.
- No cross-repo writes: everything resolves under the discovered repo root.
- Do not weaken any existing test to make yours pass. If an existing test
  fights you, your design is off — re-read §3.

## 7. Acceptance — the final regulation check

- [ ] `toimija handoff "note"` inside a `run` session appends a v1 record;
      `--show <id>` round-trips it; both `--json` forms parse.
- [ ] Record matches `docs/schemas/handoff-record.v1.schema.json` exactly
      (enforced by test), carries `version: 1`, the consistency token, and
      capped lists with honest totals.
- [ ] Closed session, missing env, traversal id, missing file: each refuses
      with a clear, tested message. Nothing is written on any refusal path.
- [ ] `toimija sessions` (text and `--json`) surfaces the record count.
- [ ] `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` ·
      `cargo test` · `toimija doctor --strict` — all green at every commit.
- [ ] README updated; help texts pass the `cli_help.rs` sweeps; five commits,
      each one wheel.

When every box ticks, the movement runs. Sign it with the PR.
