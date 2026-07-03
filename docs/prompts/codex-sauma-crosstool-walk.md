# Toimija — Sauma-why: walk and render the cross-tool chain

You are Codex, working in the `toimija` Rust crate. The sauma-why walker already
ingests `.toimija/runs/*.jsonl` and verifies top-level envelopes, but a live
test showed it surfaces only toimija's own artifacts, not kisko's model-call
events, because kisko placed its envelope under `payload` and digested only the
payload. Kisko is being fixed to top-level placement + whole-event digest
(proposal 0003 §9, D-P1/D-P2). Your job is the toimija half: make the walker
ingest, verify, and **render the cross-tool chain** once kisko conforms, and
prove it with a test that does not depend on kisko being present.

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy
-p xtask --all-targets -- -D warnings`, `cargo test`, `toimija doctor --strict`
green at **every commit**. Read `xtask/src/main.rs` before editing.

Claim first: `gh issue create` titled "Sauma-why: cross-tool chain",
`gh issue edit <n> --add-assignee @me`, branch `feat/sauma-crosstool-walk` from
a fresh `origin/main`.

## What's already right (don't regress)

`collect_artifact` (`xtask/src/main.rs` ~217) parses each artifact with
`serde_json`, reads the **top-level** `sauma`, recomputes
`digest_json(to_string(value − sauma))`, and only keeps artifacts whose digest
verifies. `scan_artifacts` already reads each `.toimija/runs/*.jsonl` line by
line (~200). Once kisko writes a top-level `sauma` with a whole-event digest
(its D-P1/D-P2), those lines will verify and be collected. Keep that path.

## Locked decisions

- **W-1 — drop the dead whole-file re-parse.** After the per-line loop,
  `scan_artifacts` also calls `collect_artifact(&raw, …)` on the entire
  multi-line jsonl file (~211). `serde_json::from_str::<Value>` on a multi-line
  jsonl always fails and early-returns, so it is dead code that also invites the
  reader to think whole-file parsing is supported. Remove it; the per-line loop
  is the complete path.
- **W-2 — render the chain as connected hops, cross-tool.** The current output
  prints each touching artifact on its own line. Make the walk show the
  **linkage**: when a `kisko-model-call.v1` artifact's `anchored_at` (or a token
  provenance edge) equals a `toimija-context.v1` artifact's token, render them as
  one chain — e.g.
  `toimija-context.v1 <digest> (…, <token>)` then a child line
  `  ← kisko-model-call.v1 <digest> (…, anchored <token>)`. The reader must be
  able to see "this model call ran against that packet" in one glance. Keep it
  transitive (a hop's target is another artifact's source) and keep verifying
  every hop's digest — an unverifiable artifact is reported as such, never
  silently walked.
- **W-3 — a cross-tool test that does not need kisko.** Add a fixture: write a
  temp `.toimija/` with (a) a real toimija packet (via the existing test
  helpers) and (b) a synthetic `runs/x.jsonl` line shaped exactly like kisko's
  fixed output — top-level `sauma` with `schema: "kisko-model-call.v1"`,
  `anchored_at` = the packet's token, and a digest computed the D-P2 way (whole
  line minus sauma). Assert `sauma-why <that token>` prints BOTH artifacts as one
  verified chain, and that a tampered digest on the synthetic line is reported
  unverifiable, not walked. This pins the contract kisko must meet, in toimija's
  own suite, so the two tools can't silently drift again.
- **W-4 — proposal 0003 status note.** Append a one-line status under §9 once the
  walk is proven ("cross-tool walk lands <date>; gate D-P3 met when kisko's
  placement fix merges").

## Forbidden moves

- No new runtime dependency beyond `serde_json` (already in xtask from the last
  fix). No graph DB, daemon, index, persistence — still a throwaway instrument.
- Do not weaken digest verification: every rendered hop is verified or flagged.
- Do not special-case kisko by tool name — key off `schema` and the
  token/anchored_at linkage, so any future producer that obeys D-P1/D-P2 joins
  the walk for free.
- Keep the walker's clarity; stay near the D-E5 spirit (small, readable).

## Acceptance

- [ ] Dead whole-file re-parse removed (W-1).
- [ ] `sauma-why <token>` renders a connected, verified `toimija-context.v1 ←
      kisko-model-call.v1` chain when both artifacts share the token (W-2),
      proven by the kisko-free fixture test (W-3).
- [ ] A tampered synthetic event digest is reported unverifiable, not walked.
- [ ] Producer-neutral: linkage keys off schema + token, not tool name.
- [ ] proposal 0003 §9 status note appended (W-4).
- [ ] fmt · clippy `-D warnings` (incl. xtask) · test · `doctor --strict` green
      at every commit.

When kisko's `prompts/codex_sauma_placement.md` and this both land, run
`sauma-why <token>` over a repo that hosted a live kisko review — if it prints
the two-tool chain and it reads like provenance, sauma graduates (proposal 0003
§9 D-P3).
