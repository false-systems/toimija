# Toimija — Sauma Retrofit + Fingerprint Blessing (issue #86)

You are Codex, working in the `toimija` Rust crate. Implement the **toimija
half of the sauma experiment** (proposal 0003 §6) plus the **#81 decision**.
Work like a Bernese clockmaker: the decisions below are locked; your craft is
the fit. Read the actual struct before writing a field name — never guess an
identifier. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, and `toimija doctor --strict` green at **every commit**.

---

## 0. Load context first (do not skip)

If `$TOIMIJA_LIVE` is set, read it first. Then read, in order:

- `docs/proposals/0003-sauma-attested-artifacts.md` — the design you are
  implementing §6 of. §4's trap is law: **no database, no daemon, no index.**
- Issue #81 — the convention question this prompt decides (option 2).
- `src/consistency.rs` — the fingerprint. Note `is_toimija_output` (~line
  305): it excludes exactly `current.md`, `current.json`, `sessions/` under
  `.toimija/`. Note the tests
  `toimija_generated_output_does_not_make_a_packet_stale` and
  `a_repo_yaml_change_is_still_a_real_change` — the second is the invariant
  you must not break. `sha2` is already a dependency; reuse the hashing
  conventions here (`sha256:`-prefixed lowercase hex).
- `src/context.rs` — `ContextPacket` (`session: Option<SessionBlock>` was
  added the same additive way you are about to add the envelope) and its
  construction site plus the three test initializers that will need the new
  field.
- `src/handoff.rs` — `HandoffRecord` (v1, schema-tested), `write()`,
  `read_all()`. `docs/schemas/handoff-record.v1.schema.json` and the
  agreement test `handoff_schema_matches_emitted_record_fields` in
  `tests/docs.rs`.
- `src/refresh.rs` — `compute()` is where the packet is finalized before
  rendering (the session block is injected there; the envelope goes in the
  same place).
- `xtask/src/main.rs` — the dev-only crate (excluded from the shipped binary
  by `default-members`); your walker lives here, keeping toimija's own scope
  untouched (proposal decision D-S3 spirit: nobody's product surface grows).

## 1. Locked decisions

- **D-R81 — the fingerprint blesses `.toimija/`.** `is_toimija_output`
  becomes: every path under `.toimija/` is excluded from the consistency
  fingerprint **except `.toimija/repo.yaml`**, which must keep counting
  (quote the existing test). Any harness writing under `.toimija/` (kisko's
  `runs/`, future tools) is then safe by construction — the class of
  self-invalidation bug found live on 2026-07-02 becomes impossible, not
  just gitignored away. Update the doc comment, README's consistency
  paragraph, and `docs/joskus-contract.md` accordingly.
- **D-E1 — the envelope is one additive field named `sauma`.** Artifacts
  gain:

  ```json
  "sauma": {
    "envelope": "sauma-envelope.v1",
    "schema": "<artifact schema id>",
    "digest": "sha256:<see D-E2>",
    "provenance": [ {"kind": "git", "oid": "..."},
                    {"kind": "token", "token": "sha256:..."},
                    {"kind": "artifact", "digest": "sha256:..."} ],
    "producer": {"tool": "toimija", "version": "<CARGO_PKG_VERSION>", "run": "<session/run id or null>"},
    "anchored_at": "sha256:<consistency token>"
  }
  ```

  Serde: `#[serde(default, skip_serializing_if = "Option::is_none")]` on the
  carrying field — repo artifacts without it stay byte-identical, old
  readers ignore it, old records still parse (write the tolerance test).
- **D-E2 — digest rule (kills the self-reference).** The digest is sha256
  over the artifact's JSON **serialized without its `sauma` field** (same
  move as a git commit hashing its content, not itself). Implement as:
  build the artifact struct with `sauma: None`, serialize with
  `serde_json::to_string` (struct field order is the canonical order —
  document that in the schema), hash, then attach the envelope. A test must
  prove: strip `sauma`, re-serialize, re-hash → equals the embedded digest.
- **D-E3 — which artifacts, which edges (this PR):**
  - **Context packet** (`toimija-context.v1` as its schema id — the packet
    finally gets an explicit schema; kisko's `RawPacket` already reads an
    optional top-level `schema` field, so ALSO emit top-level
    `"schema": "toimija-context.v1"` — two birds, kisko's banner stops
    printing `schema: unknown`): provenance `[git head oid]`;
    `anchored_at` = consistency token; `producer.run` = session id on
    per-session packets, null on repo-level.
  - **Handoff record** (`handoff-record.v1`, unchanged version — additive):
    provenance `[token edge (the record's consistency_token), git edge
    (head)]`; `anchored_at` = same token; `producer.run` = session id.
  - Kisko's transcript events are the third artifact — **not this repo**;
    the kisko Phase A prompt carries that half.
- **D-E4 — schema files are normative.**
  `docs/schemas/sauma-envelope.v1.schema.json` (draft 2020-12, `$id`,
  described properties, `required`, `additionalProperties: false`, evolution
  rules in the description — mirror `handoff-record.v1.schema.json`'s
  conventions exactly). Update `handoff-record.v1.schema.json` additively
  (optional `sauma` property referencing the envelope schema). Extend the
  agreement test so the emitted envelope's field set matches the schema —
  hand-rolled comparison, **no new dependencies**.
- **D-E5 — the walker is a dev-only xtask.** `cargo xtask sauma-why <digest
  | token | session-id>`: synchronously scans this repo's known artifact
  locations (`.toimija/current.json`, `.toimija/sessions/*/current.json`,
  `.toimija/sessions/*/handoff.jsonl`, `.toimija/runs/*.jsonl` if present),
  builds the edge list from `sauma.provenance` + `anchored_at` in memory,
  and prints the chain touching the query — each hop as
  `<schema> <short digest> (producer, anchored token) ← edge`. On-demand
  view, no persistence, no index (proposal §4). This is the experiment's
  instrument, not a product: keep it under 200 lines, favor clarity.

## 2. Order of work — four commits, each gated

Claim first: `gh issue edit 86 --add-assignee @me`. Branch
`feat/86-sauma-retrofit` from a **fresh** `origin/main`.

1. **D-R81** — the fingerprint blessing: `is_toimija_output` rewrite, tests
   (runs/ content does not move the token; `repo.yaml` still does — extend
   the two named tests), doc updates. This commit also closes #81 — say so
   in its message body (`Closes #81` goes in the PR, reference in commit).
2. **Envelope on the packet** — `SaumaEnvelope` type (probably in a new
   small `src/sauma.rs`; `context.rs` and `handoff.rs` both use it), packet
   integration in `refresh::compute` (after the session block injection),
   top-level `schema` field, digest round-trip test, initializer fixes.
3. **Envelope on the handoff record** + schema files (D-E4) + agreement-test
   extensions + old-record tolerance test.
4. **The walker** (D-E5) + a short `docs/proposals/0003` §6 status note
   appended ("retrofit landed <date>, walker: `cargo xtask sauma-why`") +
   README one-liner in the packet-includes list.

Then: rebase on `origin/main`, PR (`Closes #86`, `Closes #81`; body names
D-R81 and D-E1..E5 and quotes the digest round-trip evidence), merge when
green, `git pull && cargo install --path .`.

## 3. Forbidden moves

- No graph database, no daemon, no index, no persistence for the walker.
- No new dependencies (sha2 and serde are already present; the schema test
  is hand-rolled).
- No breaking change to any schema: everything additive; `handoff-record`
  stays v1; old fixtures must still decode (tested).
- Do not weaken `a_repo_yaml_change_is_still_a_real_change` — if the
  blessing fights it, the blessing is wrong, not the test.
- Do not touch kisko or its transcript events from this repo.
- No editing proposals 0001–0003 beyond the §6 status note in commit 4.

## 4. Acceptance

- [ ] Content under `.toimija/runs/` and `.toimija/sessions/` never moves
      the token; `.toimija/repo.yaml` always does — tested.
- [ ] Packet and handoff record carry `sauma` envelopes; digest round-trip
      test proves D-E2; repo-level packet without envelope? No — packet
      always carries it now; **byte-parity concern is versioned-contract
      concern**: kisko's parser tolerates unknown fields (verify by running
      `kisko check` locally if installed; at minimum the raw-Value parse
      path in the contract doc).
- [ ] `docs/schemas/sauma-envelope.v1.schema.json` exists, normative,
      agreement-tested; handoff schema updated additively.
- [ ] `cargo xtask sauma-why <token>` on this repo prints a real chain
      (packet ← handoff hops) — paste one run's output into the PR body.
- [ ] fmt · clippy `-D warnings` · test · `doctor --strict` green at every
      commit; four commits, each one wheel.

When every box ticks, the movement runs. Sign it with the PR.
