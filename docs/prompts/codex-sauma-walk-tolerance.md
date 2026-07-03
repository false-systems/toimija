# Toimija — Sauma-why: tolerate open edge kinds + link on shared anchor

You are Codex in the `toimija` crate. The live cross-tool walk (proposal 0003
§9 D-P3) ran end to end and **caught two real interop gaps the unit fixture
missed** — kisko now writes a top-level envelope anchored to the packet token,
but `sauma-why` (a) crashes on kisko's provenance edge kinds and (b) would not
link kisko's event to the packet even if it parsed. Fix the walker to be
producer-neutral. No kisko change — the seam tolerates producers, not the other
way round. `cargo fmt --check`, clippy `-D warnings` (incl. `-p xtask`),
`cargo test`, `toimija doctor --strict` green at every commit.

Claim first: `gh issue create` titled "sauma-why: open edge kinds + anchor
linking", assign `@me`, branch `fix/sauma-walk-tolerance` from fresh
`origin/main`.

## What the live walk showed (verified)

A real kisko `review` transcript event carries:
`"sauma": { "schema": "kisko-model-call.v1", "anchored_at": "<packet token>",
"provenance": [ {"kind":"context_fragment", …}, {"kind":"contract", …} ] }`.
Two failures follow:

- `cargo xtask sauma-why <token>` → `Error: unknown variant 'context_fragment',
  expected one of 'git', 'token', 'artifact'`. The `Provenance` enum
  (`xtask/src/main.rs:118`, `#[serde(tag="kind", rename_all="snake_case")]`) is
  a **closed** vocabulary; kisko's `context_fragment`/`contract` kinds abort the
  whole walk at deserialize time.
- The packet token lives **only in the event's `anchored_at`**, never in a
  provenance edge. `links_to` (`xtask:202`) only inspects `child.edges`, so the
  event cannot become a child of the packet.

## Locked decisions

- **F-1 — open edge kinds.** Provenance is an **extensible** vocabulary: a
  walker must never crash on an edge kind it doesn't model. Make `Provenance`
  tolerant — keep the typed `git`/`token`/`artifact` variants for their linking
  value, and add a catch-all that captures any other `{kind, …}` edge without
  failing (e.g. `#[serde(other)] Other` plus a lenient parse that still extracts
  a linking string — `digest`/`token`/`oid`/`id`/`ref` — when the edge object
  carries one; an edge with no id-like field simply contributes no link, never
  an error). After this, an unknown kind from any future producer degrades to
  "no edge" at worst, never a crash.
- **F-2 — link on the shared anchor, not just edges.** Two artifacts anchored to
  the **same consistency token** are about the same repo state and belong on the
  same chain. Extend `links_to` so a child links to a parent when
  `child.anchored_at == parent.anchored_at` (both non-empty), **in addition to**
  the existing edge→digest / edge→parent-anchor rules. This is what makes a
  kisko model-call event (anchored to the packet token) render as
  `← kisko-model-call.v1` under the `toimija-context.v1` packet without kisko
  having to duplicate the token into an edge. Keep it producer-neutral: key off
  `anchored_at` + `schema`, never a tool name. Guard against self-links and
  cycles (the existing `seen` set) and against two packets with the same token
  linking to each other (only non-context children hang under a context parent,
  or add a stable ordering so the packet stays the root).
- **F-3 — the fixture must match reality.** The current
  `renders_cross_tool_chain_and_reports_tampering` test builds a synthetic kisko
  event that put the token in an edge — that is why it passed while the live
  walk failed. Rebuild the fixture event to mirror what kisko actually emits:
  the token **only in `anchored_at`**, provenance kinds `context_fragment` and
  `contract` (no token edge). The test must now prove: (a) that realistic event
  links under the packet by shared anchor; (b) a `context_fragment`/`contract`
  edge kind does not crash the walk; (c) a tampered digest still refuses to
  render as a verified child. This fixture is the contract kisko must keep — if
  it drifts from a real transcript again, this test is where we catch it.
- **F-4 — confirm the digest actually verifies cross-tool.** Once F-1 lets the
  event parse, the walker recomputes `digest_json(to_string(event − sauma))` and
  compares to kisko's embedded `sha256_json(event − sauma)`. Add an assertion
  (in the fixture, built with the same compact serde form kisko uses) that a
  faithfully-constructed event **verifies** — so we know the two tools'
  canonicalization matches byte-for-byte, not just that parsing succeeds.
- **F-5 — record the vocabulary rule.** One line in proposal 0003 §9 (and the
  `sauma-envelope.v1` schema description): provenance `kind` is an **open**
  enumeration; the load-bearing part of an edge is its linking value; walkers
  MUST tolerate unknown kinds. Keep `additionalProperties`/`oneOf` in the schema
  permissive enough that `context_fragment`/`contract` validate.

## Forbidden moves

- No kisko change; no new dependency; no graph DB/daemon/index/persistence.
- Never render an unverified or digest-mismatched artifact as a verified hop.
- Don't special-case by tool name — F-2 keys off `anchored_at` + schema.

## Acceptance

- [ ] `sauma-why <token>` no longer crashes on `context_fragment`/`contract`
      edges (F-1).
- [ ] A kisko event anchored to the packet token renders as
      `← kisko-model-call.v1` under the packet by shared anchor (F-2), proven by
      the reality-matching fixture (F-3).
- [ ] A faithfully-built kisko event verifies its digest cross-tool (F-4); a
      tampered one is refused.
- [ ] proposal 0003 §9 + schema note the open-kind rule (F-5).
- [ ] fmt · clippy `-D warnings` (incl. xtask) · test · `doctor --strict` green.

When this lands, re-run the live walk (`toimija run -- kisko review`, then
`sauma-why <token>`). If it prints the two-tool chain verified, proposal 0003
§9 D-P3 is met and the sauma crate is justified.
