# Toimija — Sauma retrofit review fixes (post-merge #88)

You are Codex, working in the `toimija` Rust crate. The sauma retrofit (#88)
merged, then an 8-angle review found that the envelope shape is right and
kisko-compatible, but **the two things that make sauma worth having are broken:
the packet digest can't be reproduced from the file it attests, nothing ever
verifies the digest, and the `sauma-why` walker doesn't actually walk.** Fixing
these is also what unblocks the proposal-0003 experiment gate — after this
lands, `sauma-why` on one real week's chain is the go/no-go for the crate.

Work like a clockmaker: read the struct before writing a field name. `cargo fmt
--check`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy -p xtask
--all-targets -- -D warnings`, `cargo test`, `toimija doctor --strict` green at
**every commit**. Line numbers are from merged main `edf119f` and may shift.

Claim first: `gh issue create` titled "Sauma retrofit review fixes", `gh issue
edit <n> --add-assignee @me`, branch `fix/sauma-review` from fresh `origin/main`.

## Locked decisions

- **T-1 — the digest must be reproducible from the on-disk artifact (HIGH).**
  `attach_sauma` (`src/context.rs:91`) digests `serde_json::to_string` (compact),
  but `src/refresh.rs:155` persists with `to_string_pretty` — so stripping
  `sauma` from the on-disk `current.json` and re-hashing never reproduces the
  embedded digest. Define a **canonical serialization** and use it on both the
  hashing side and any verifier: the canonical form is **compact
  `serde_json::to_string` of the typed struct with `sauma` set to `None`**
  (UTF-8, serde field order, `skip_serializing_if` omissions). Keep the on-disk
  file pretty for humans — but document precisely, in
  `docs/schemas/sauma-envelope.v1.schema.json`'s description, that the digest is
  over this canonical compact form, so any consumer re-parses into the type and
  re-serializes compact before hashing. **New test that exercises the real
  path:** read the persisted pretty `current.json` from disk → deserialize →
  strip `sauma` → canonical-serialize → hash → assert equals the embedded
  `sauma.digest`. (The current round-trip test hashes in-memory compact and so
  never catches this.)
- **T-2 — verify the digest somewhere (HIGH).** Today nothing recomputes it:
  `toimija verify` checks only the consistency token; `sauma-why` trusts the
  digest as-read. Add digest verification: extend `toimija verify` (or add
  `toimija sauma verify <path>`) to strip+canonical-serialize+rehash the packet
  and handoff records and compare against the embedded digest, reporting a
  tamper mismatch. The walker (T-4) must also verify each hop's digest as it
  walks. A forged envelope with an arbitrary digest must no longer pass.
- **T-3 — unify the "generated output" predicate so watch and consistency can't
  drift (MED-HIGH).** `src/watch.rs:131` `is_ignored()` still ignores only
  `current.*` + `sessions/`, while `is_generated_output` in `src/consistency.rs`
  now blesses all of `.toimija/` except `repo.yaml`. A harness writing
  `.toimija/runs/*.jsonl` triggers a refresh that recomputes an identical token
  (churn/thrash). Make `watch::is_ignored` delegate to the SAME predicate
  `consistency` uses (export it `pub(crate)`), so the two lists are one list.
  Add a test: a write under `.toimija/runs/` is ignored by watch AND does not
  move the token.
- **T-4 — make `sauma-why` a real, verified transitive walk (MED).** Rewrite the
  walker (`xtask/src/main.rs`). Add `serde_json` to `xtask/Cargo.toml` (the main
  crate already depends on it; the no-new-deps rule was scoped to shipped src,
  and this is the dev-only instrument) and delete the hand-rolled
  `object_after`/`balanced`/`json_string`/`provenance_edges` scanner — it has
  real bugs (`find("\"sauma\"")` grabs the wrong object when an earlier field
  contains the quoted word; `balanced()`'s escaped-quote flag is inverted so an
  escaped `\"` terminates the string). Parse each artifact into a
  `#[derive(Deserialize)]` mirror of the envelope. Then: build the provenance
  edge graph (digest → the digests/tokens/oids it derives from), and from the
  query do an actual transitive walk (BFS/DFS hop-to-hop), **verifying each
  hop's digest** (T-2) as you go, printing the chain in order. Match the query
  against full digests/tokens/ids, not `.contains` substring — a 6-hex or
  session-id query must not pull in coincidental matches. Keep it dev-only, no
  persistence, no index; aim back under the D-E5 200-line ceiling now that
  serde_json does the parsing.
- **T-5 — `schema` field gets `#[serde(default)]` (MED).** `src/context.rs:24`
  added `schema` as a required field while `session` and `sauma` are
  `Option`+`default`. A future reader of a pre-#88 packet hard-fails on missing
  `schema` but tolerates missing `session`/`sauma` — asymmetric evolution. Add
  `#[serde(default = "...")]` (defaulting to the current `CONTEXT_SCHEMA`) so all
  three evolve the same way.
- **T-6 — attach the envelope on the `context --json` path (MED).** `toimija
  context --json` (`src/cli.rs:508`) prints `build_context_packet` output
  directly with `sauma: None`, so a first-class emission is `schema`-tagged but
  unattested, contradicting the README's "the packet includes a sauma envelope."
  Call `attach_sauma(None)` before printing (run = None for the ephemeral path),
  so every emitted packet carries the envelope.
- **T-7 — the agreement test must check nested shape, and `$id` should be an
  absolute URI (LOW-MED).** `tests/docs.rs` compares only top-level key names, so
  drift in `producer`/`provenance` (an added/renamed sub-field, a broken
  `const`/`pattern`, `additionalProperties`) goes uncaught. Recurse the emitted
  envelope against the schema's nested `required`/`properties`/`additional
  Properties` (hand-rolled, still no runtime schema-validator dep). Make the
  schema `$id`s absolute URIs (e.g. `https://false.systems/schemas/…`) and the
  handoff `$ref` resolvable, so a consumer loading the handoff schema standalone
  can resolve the sauma `$ref`.

## Cleanup (fold into the relevant commit)

- Collapse the duplicated `attach_sauma` strip-serialize ritual in `context.rs`
  and `handoff.rs` into one shared helper (they differ only in provenance
  assembly).
- Have `sauma::digest_json` call a `pub(crate) hash_bytes` exported from
  `consistency.rs` instead of re-implementing the `sha256:<hex>` primitive — one
  content-address helper for the crate. (`sha2` is already a dependency; no new
  dep.)

## Forbidden moves

- No runtime JSON-schema-validator dependency (the agreement test stays
  hand-rolled). `serde_json` in xtask is the only new dep, and only there.
- No graph database, daemon, index, or persistence for the walker.
- Do not weaken `a_repo_yaml_change_is_still_a_real_change` or
  `toimija_generated_output_does_not_make_a_packet_stale`.
- Handoff record stays v1; all schema changes additive; old records still parse.

## Order of work — one commit per lettered fix (or grouped), each gated

Suggested: (1) T-1 canonical digest + real on-disk test; (2) T-2 verification +
T-3 unified predicate; (3) T-4 walker rewrite; (4) T-5+T-6 serde fixes; (5) T-7
agreement test + schema `$id`; cleanup folded in. Then rebase, PR (`Closes
<n>`), and — the payoff — **paste a real `cargo xtask sauma-why <token>` run
that walks this repo's actual packet ← handoff chain, verified, into the PR
body.** That output is the experiment's evidence. Merge when green, `git pull &&
cargo install --path .`.

## Acceptance

- [ ] Read pretty `current.json` from disk, strip `sauma`, canonical-serialize,
      rehash → equals embedded digest (tested against the ON-DISK form).
- [ ] `toimija verify` (or `sauma verify`) catches a hand-edited/forged digest.
- [ ] `watch::is_ignored` and `consistency` share one predicate; a
      `.toimija/runs/` write is ignored AND doesn't move the token (tested).
- [ ] `sauma-why` parses with serde_json, walks provenance edges transitively,
      verifies each hop, matches full ids not substrings; PR body has a real run.
- [ ] `schema` is `#[serde(default)]`; `context --json` carries the envelope.
- [ ] Agreement test recurses into producer/provenance; schema `$id` absolute.
- [ ] fmt · clippy `-D warnings` (incl. xtask) · test · `doctor --strict` green
      at every commit.

When `sauma-why` walks the week's chain and it reads like provenance instead of
a JSON dump, the experiment has its answer.
