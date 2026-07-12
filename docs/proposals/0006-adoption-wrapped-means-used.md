# Proposal 0006 — Adoption: wrapped means used

**Status:** ACCEPTED 2026-07-11 (implementation dispatched by Yair).

**Date:** 2026-07-11

**Builds on:** [proposal 0001](0001-sessions-awareness-handoff.md) (sessions,
`run`, `intent`) and [proposal 0005](0005-workset-contract.md) (the workset
contract). No daemons, no network, no dispatch, no MCP, no plugin system.
Git hooks installed here run synchronously inside explicitly-invoked Git
commands (same execution model as the existing pre-commit dev hook); launch
adapters are plain process invocation, which `run` already owns.

---

## 1. The problem, observed live

Toimija's packets are correct, fresh, and unread.

- Agents dropped into a repo have no signal toimija exists unless a
  hand-maintained paragraph in AGENTS.md/CLAUDE.md says so — and only those
  two files are covered; Cursor, Copilot, Gemini, and Zed conventions are not.
- The paragraph itself is a *conditional program* ("if `$TOIMIJA_LIVE` is
  set… otherwise run `enter --for-diff --budget 4000`…") that the agent must
  notice, evaluate, and obey before it is even oriented.
- Worst: even `toimija run codex` — the strongest opt-in we have — delivers
  only an **environment variable**. For the packet to reach the agent's
  context, three voluntary steps must all succeed: notice the instruction,
  probe the env, read the file. Env vars are invisible by default. An env
  var is not context. Wrapped sessions have demonstrably run without ever
  consuming the packet.

The principle this proposal installs: **choosing toimija is the voluntary
act; using it is not.** A human who launches `toimija run` has opted in;
from that point consumption must be guaranteed by construction, not
requested by prose. Voluntary cooperation remains the model only *outside*
the envelope (an agent that merely finds the repo's docs).

What "not voluntary" honestly means — and does not:

- CAN: put the packet in the agent's context **by construction** (argv).
- CAN: **refuse to launch** when prerequisites are missing.
- CAN: **report consumption evidence at exit**, from toimija-owned state.
- CANNOT: observe or police the agent mid-session. Comprehension cannot be
  forced, reads cannot be watched (0005 R2 stands). Structural at the door,
  honest at the exit, never a warden in between.

## 2. The design — one argument in four parts

Delivery channels, ranked by guarantee: **argv** (deterministic — the agent
cannot not-see its launch prompt) > **auto-read entry files** (mechanical
but static) > **env var + instruction** (probabilistic; today's model).
Each part below moves adoption up that ladder.

### Part A — `toimija adopt`: own the instruction layer

`toimija adopt` writes one canonical preamble block into every agent entry
file present in the repo — `AGENTS.md`, `CLAUDE.md`, `.cursorrules`,
`.github/copilot-instructions.md`, `GEMINI.md`, `.rules` (Zed) — between
markers, idempotently. The preamble is one flat imperative, no branches:

> Read `.toimija/current.md` before reading anything else in this repo; it
> is your workset contract. Under a toimija session your launch prompt
> names your live packet — that file supersedes this one.

`toimija doctor --strict` gains an adoption drift check: preamble present
and current in every entry file that exists. Same generated-prose-plus-
checker mechanism the boundaries mirror already proves.

### Part B — freshness without being asked

`toimija adopt --hooks` installs Git hooks (post-checkout, post-merge,
post-commit) that run `toimija refresh` synchronously, so
`.toimija/current.md` is always true and Part A's flat instruction never
points at stale state. Hooks live in `.git/` (untracked), installed
per-clone like the existing pre-commit hook; the hook is a no-op outside a
toimija-configured repo. `adopt --hooks` also prints (not installs)
harness-side snippets — e.g. a Claude Code SessionStart hook that emits the
packet straight into session context — for the human to place; toimija does
not edit files outside the repo.

### Part C — guaranteed delivery under `run`

`toimija run` gains launch adapters for known agents (`claude`, `codex` —
both accept a positional prompt). The adapter composes the launch prompt:
packet path, the one-line contract framing, and the human's task
(`--task "<text>"`), then execs the agent with it. The packet is in context
by construction — delivery through the only channel with a guarantee.
There is precedent for the shape: `src/adapters/` already holds per-language
adapters; these are the same idea aimed at harnesses, and they are argv
composition only — no protocol, no IPC, no SDK.

Unknown agents fall back to today's env-var behavior with an honest launch
banner: `delivery: env-var only — not guaranteed for '<agent>'`. Toimija
never silently degrades.

### Part D — prerequisites and the exit report

- `run` requires intent: no `--intent` ⇒ refuse to launch, with the fix in
  the error message. Declaring why the session exists is the entry fee of
  the envelope (an explicit `--intent unknown` remains possible — visible
  honesty over silent absence).
- `run`'s exit summary (it already diffs Git state entry→exit) gains a
  consumption report, built entirely from toimija-owned state: intent
  declared or launch-provided, scope expansions declared (count), handoff
  written or not, packet refreshes during the session. Non-use becomes
  visible to the human instead of silent; prompts get fixed on evidence.

### Minor — the payoff line

The packet header gains one provenance line ("compiled from N tracked
files, M commits of bounded history, K coupling facts") so the packet
demonstrates, not just asserts, that reading it is cheaper than rediscovery.

## 3. Explicitly rejected

- **R1 — bespoke editor/harness integrations** (Zed extension, Cursor
  plugin, MCP context server). Files and argv are the API; universality is
  the product. Revisit a single MCP server only on observed evidence that
  agents miss the packet *despite* Parts A–C being in place — as its own
  boundary-change proposal, pulled by failure, not pushed by plausibility.
- **R2 — read telemetry / mid-session policing.** Reaffirmed from 0005.
  The exit report reads toimija's own artifacts only — never the agent's
  behavior, never file access.
- **R3 — auto-editing files outside the repo.** Harness configs
  (`~/.claude/settings.json` etc.) are printed as snippets, never written.
  Toimija's write surface stays inside the repository.
- **R4 — a second instruction dialect per agent brand.** One canonical
  preamble, adapted only in placement, never in content. Divergent
  per-agent prose would rot exactly like the hand-maintained paragraphs
  this proposal replaces.

## 4. Decisions

- **D1 — wrapped means used.** Inside the `toimija run` envelope, packet
  delivery is structural (argv), not behavioral (instructions). Voluntary
  compliance is the model only outside the envelope.
- **D2 — `adopt` owns the entry files.** Canonical marker-delimited
  preamble, idempotent, all known agent conventions, drift-guarded by
  `doctor --strict`.
- **D3 — the instruction is flat.** One unconditional imperative pointing
  at `.toimija/current.md`; the conditional `$TOIMIJA_LIVE` program in
  AGENTS.md/CLAUDE.md is retired when Parts B–C land.
- **D4 — freshness is hook-driven and synchronous.** Git hooks per-clone,
  opt-in via `adopt --hooks`; harness snippets printed, not installed.
- **D5 — launch adapters for `claude` and `codex` first.** Unknown agents
  get the honest degraded banner, never silent fallback.
- **D6 — `run` refuses to launch without intent.** `--intent unknown` is
  the explicit escape hatch.
- **D7 — the exit report is toimija-state only.** Intent, declared
  expansions, handoff, refresh count. Nothing observed about the agent.

## 5. Resolutions

- `adopt` amends existing entry files and creates `AGENTS.md` only when no
  known entry file exists at all. Adoption is never a silent no-op.
- Known launch adapters inline the packet body at the session budget and also
  pass the live packet path. The path remains available for re-reads after Git
  state changes; argv limits are comfortably above the packet budget.
- Delivery is split into two PRs: PR 1 contains this accepted proposal,
  `adopt`, hooks, the doctor drift check, and README changes. PR 2 branches
  from `main` after PR 1 and contains launch adapters, `--task`, intent
  enforcement, the exit report, provenance, and the dogfooded preamble swap.
