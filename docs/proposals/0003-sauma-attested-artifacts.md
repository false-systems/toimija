# Proposal 0003 — Sauma: attested artifacts across the stack

**Status:** ACCEPTED in direction 2026-07-02 (Yair, design session; **Rust,
not Vaisto** — decision D-S2). Implementation gated on the §6 retrofit
experiment.

**Scope:** cross-repo — toimija and kisko emit; a small standalone crate
walks. Counterpart context: kisko `docs/proposals/0001-interactive-read-loop.md`
§9 ("the Vaisto seam").

*Sauma* — Finnish for **seam**. The layer between the layers.

---

## 1. What the stack really makes

Strip each tool to its essence and one shape repeats:

- **toimija** makes checkable claims about *repo state*: "the repo is exactly
  X" (consistency token), and can freeze X (snapshot trees + leases).
- **kisko** makes checkable claims about *model interactions*: "the model saw
  exactly Y, at snapshot X, audited before visible; it produced Z under
  contract C."
- **handoff records** are checkable claims about *sessions*, anchored to a
  token.
- **reviews** produce claims about claims (findings + verifier verdicts).

Every layer manufactures **evidence** — in four private dialects. Meanwhile
the actual provenance of a week's work (bug report → issue → fix → observed
gap → design → proposal → build → review → amended spec) exists only in
prose and in whichever agent is still in the room. When the session ends,
the graph evaporates into commit messages.

**What the stack really makes is a chain of custody for work done by
agents.** Git gave code a verifiable history; sauma gives the *thinking
around the code* one.

## 2. The abstraction: one envelope

Everything that crosses a layer boundary wears the same envelope
(`sauma-envelope.v1`, JSON fields — see §5: a convention, not a library):

```json
{
  "schema": "handoff-record.v1",
  "digest": "sha256:<over the stored content bytes>",
  "provenance": [
    {"kind": "artifact", "digest": "sha256:…"},
    {"kind": "git", "oid": "<tree or commit>"},
    {"kind": "token", "token": "sha256:…"},
    {"kind": "external", "ref": "github:false-systems/kisko#10"}
  ],
  "producer": {"tool": "kisko", "version": "0.1.0", "run": "<id>"},
  "anchored_at": "sha256:<consistency token, when repo-anchored>"
}
```

- **`schema`** — every artifact already versions per the standing rule
  (handoff-record.v1, loop-protocol.v1, …). Sauma adds no new obligation.
- **`digest`** — content-addressed over the stored bytes as stored (Git's
  rule; no canonical-JSON scheme, no normalization pass).
- **`provenance`** — edges to what this artifact was derived from. The graph
  IS these fields; there is no other data structure.
- **`anchored_at`** — the consistency token, for artifacts that assert
  something about a repo state.

Existing conformance is already high: the packet is versioned and
token-stamped; snapshots are git objects (content-addressed by
construction); handoff-record.v1 embeds the token; kisko transcripts are
producer-stamped event streams. **The missing piece is only `digest` +
`provenance`.**

## 3. What it unlocks

1. **`git blame` for decisions.** "Why does this code exist?" → walk
   provenance: commit ← agent PR ← implementation prompt ← proposal ←
   design session ← a bug found in a live transcript at token `9e24a700…`.
   Survives every session end — cross-agent memory as data, not notes.
2. **Replay and eval.** A model answer's full input set is content-addressed:
   re-run the same contract and context on a different model, diff the
   outputs. The cheap-model thesis becomes measurable on identical evidence.
3. **Transitive trust.** Kisko verified the packet; everything downstream
   inherits checkable lineage. The cheaper the model, the more the chain
   matters — you trust the custody, not the model.
4. **Storage already exists.** Git objects (+ toimija's lease/GC machinery)
   for anything that must be pinned; JSONL where streams already live. No
   new infrastructure.

## 4. The trap, named

`repo.yaml` says `not: graph database` — a confirmed boundary, not a vibe.
Sauma therefore is: **no database, no daemon, no service, no index.**
Artifacts stay in the files and git objects where they already live; the
graph is digests-in-fields; every traversal is a synchronous command
computing a view on demand (`sauma why <digest>`), the same trick that kept
`toimija sessions` inside the boundaries.

## 5. Decisions

- **D-S1 — envelope is a convention first.** v1 is a spec (schema docs +
  JSON fields), not a shared library. Toimija and kisko conform by writing
  fields; **zero code coupling between emitters**. Drift is caught the same
  way as handoff-record.v1: schema files + agreement tests per repo.
- **D-S2 — Rust, not Vaisto** (Yair, 2026-07-02). The typed layer is serde
  types in a small standalone crate + CLI (`false-systems/sauma`): envelope
  types, digest helpers, and the walker. Vaisto remains an authoring-layer
  possibility (contracts written in Vaisto could *emit* sauma artifacts) and
  is never a runtime dependency of any tool in the stack.
- **D-S3 — the walker is a separate binary.** `sauma why <digest>`,
  `sauma log <path|token>` live in the sauma crate — toimija's and kisko's
  scopes stay untouched; neither gains a dependency in v1.
- **D-S4 — schemas under the emitting repo.** Each artifact's schema stays
  where its emitter lives (kisko owns model-answer.v1; toimija owns
  handoff-record.v1); the envelope spec itself lives in the sauma repo, with
  copies validated by each repo's agreement tests.
- **D-S5 — additive retrofit.** `digest`/`provenance` fields are optional,
  additive within existing schema versions per the standing evolution rule.
  Old artifacts remain readable; the graph simply has fewer edges into the
  past.

## 6. The gate: a week-sized experiment before any commitment

Do not build the crate first. Retrofit the envelope onto the three artifacts
that already exist —

1. toimija's context packet (`digest` + empty provenance),
2. `handoff-record.v1` (+`provenance`: packet digest, session id),
3. kisko transcript events (+`digest`/`provenance`: fragment digests on
   model calls, tree OID on reads)

— then hand-walk one real week of work with a throwaway script. **If the
walk of a real chain (bug → issue → fix → proposal → build → review) reads
like magic, build the sauma crate. If it reads like bookkeeping, we lost
three optional fields and stop.**

Status note, 2026-07-03: the toimija retrofit landed for context packets and
handoff records; the throwaway walker is `cargo xtask sauma-why`.

## 7. Boundary notes

No boundary edits required for §6: each retrofit evolves a repo's own
artifact schemas within existing scope. If toimija or kisko ever *depends*
on the sauma crate (post-experiment), that is a manifest + AGENTS.md scope
note per the drift guard — a decision, recorded, like every other one.

## 8. Non-goals

A graph database, daemon, index, or service; canonical-JSON schemes; a
shared runtime library in v1; Vaisto as a dependency; retroactive
back-filling of history (the graph starts when the fields do); any weakening
of the confirmed boundaries (no network, no dispatch, read-only kisko).

## 9. Amendment 2026-07-03 — placement is normative (the cross-tool gate)

The §6 experiment ran live: a kisko `review` in the toimija repo wrote a
`kisko-model-call.v1` envelope whose `anchored_at` equals the toimija packet
token. **The cross-tool anchor works** — kisko knows exactly which verified
repo state its model call ran against. But `cargo xtask sauma-why <token>`
surfaced only the toimija packet, not the kisko event, because the "one
envelope" was one *shape* in two *placements*: toimija writes `sauma` at the
artifact's top level; kisko nested it under `payload`, and the walker reads
top-level only. The four dialects reappeared one level down.

So the envelope needs a **placement + digest law**, not just a field list.
Locked:

- **D-P1 — top-level placement.** The `sauma` envelope is a **top-level field
  on every artifact.** For a JSONL event stream, *each line is an artifact* and
  carries its own top-level `sauma`. No nesting under `payload`/`data`/etc.
- **D-P2 — whole-artifact digest.** The digest is `sha256:` over
  `serde_json::to_string(artifact)` with the **top-level `sauma` field
  removed** — the entire artifact object minus its own envelope, compact,
  with `serde_json::Value`'s default sorted-key ordering. Every producer
  computes it identically, so any walker verifies any tool's artifact by the
  same recompute. (This is what toimija already does for packet/handoff; kisko
  must match — hoist the envelope out of `payload` and digest the whole event
  line, not just the payload.)
- **D-P3 — the graduation gate is one command.** When both tools obey D-P1/D-P2,
  `sauma-why <token>` over a directory holding a toimija packet and a real
  kisko transcript must print **one verified chain spanning both tools**
  (`toimija-context.v1 ← kisko-model-call.v1`). If that reads like provenance,
  the sauma crate is justified (§6). If it is still thin, convention-only, the
  crate waits. Do not greenlight or kill until this command runs green.

Implementation split: kisko `prompts/codex_sauma_placement.md` (D-P1/D-P2 on the
transcript event), toimija `docs/prompts/codex-sauma-crosstool-walk.md` (ingest
+ verify + render the cross-tool chain). No boundary change — schema evolution
within existing scope, as §7.

Status note, 2026-07-03: cross-tool walk lands; gate D-P3 met when kisko's
placement fix merges.
