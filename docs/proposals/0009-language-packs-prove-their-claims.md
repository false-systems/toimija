# Proposal 0009 — Language packs prove what they claim

**Status:** ACCEPTED 2026-07-21 (Yair dispatched implementation).

**Date:** 2026-07-21

**Builds on:** the adapter architecture (`src/adapters/`), the minimum
language packs (`adapters/minimum.rs` — "a new language is data, not a
module"), and the trust model (#151: toimija reports what it verified, not
what it assumes). No boundary edits — no Tree-sitter, no LSP, no source
parsing, no network, no plugin code path. Everything below is manifest reads
and path checks, the same evidence class detection already uses.

---

## 1. The weak spots, named

The language layer is honest everywhere except its edges. Three gaps:

1. **Inferred commands are guesses.** The minimum packs emit `npm test`
   even when `package.json` has no `test` script, and `pytest` / `ruff
   check .` even when `pyproject.toml` configures neither. Everywhere else
   toimija claims only what it proved (armed watchers name their probe,
   companion hints are existence-filtered); the minimum packs are the one
   place it asserts something it never checked. An agent that runs the
   guessed command burns a turn on `npm ERR! missing script: test` and
   learns to distrust the plan.
2. **Root-only, single-kind detection.** A repo with `services/api/go.mod`
   and `web/package.json` but no root manifest is "generic". One repo =
   one language is the assumption most likely to break on the first
   external user.
3. **Supported languages are undiscoverable.** The answer to "what does
   toimija support and how" lives only in source. A product states its
   capabilities.

## 2. Two observations that shape the fix

**Proof is one file read away.** `package.json` is JSON and `serde_json`
already ships in the tree; `pyproject.toml` yields to the same
section-aware scan `adapters/rust.rs` already does on `Cargo.toml` (no
`toml` crate, by design). Checking whether a claimed command exists costs
one bounded read of a file detection already touched — the evidence was
always in reach; the packs just never looked.

**Detection is already a walk of length zero.** Root-manifest detection is
a degenerate bounded search. Extending it to a shallow, exclusion-honoring
walk changes its radius, not its nature — still manifests, still paths,
still no source parsing.

## 3. Design

### Part A — prove before inferring

A minimum-pack command is emitted only when its manifest evidence exists:

- **Node:** parse `package.json` (serde_json). The fast tier emits
  `npm test` only when a `scripts.test` entry exists, else drops it with a
  note; the strict tier emits `npm run <name>` for each of the scripts
  that actually exist (`lint`, then `typecheck`).
- **Python:** section-aware scan of `pyproject.toml`. `pytest` requires the
  standard `[tool.pytest.ini_options]` table or a root `pytest.ini`; a generic
  `tests/` directory is not evidence because it may use `unittest` or another
  runner. This is knowingly strict: a genuine pytest repo that declares
  pytest only as a dependency, with zero config, drops to "no fast tier"
  with a note — accepted, like 0008's NTP false-positive, because the
  failure mode is a missing suggestion, never a wrong claim. `ruff check .`
  requires a `[tool.ruff]` table or a root `ruff.toml` / `.ruff.toml`. A
  `setup.py` repo without pytest-specific evidence drops `pytest` with a
  note.
- **Go:** `go test ./...` and `go vet ./...` are defined by the toolchain,
  not the manifest — `go.mod` alone already proves them. No change.

A dropped command leaves a note in the plan, in the existing Notes tier:
`no test script in package.json — fast tier not inferred`. The plan's
`source` semantics are unchanged; `repo.yaml` validation config still
overrides everything inferred.

### Part B — secondary languages, observed and reported

`detect_repo_kind` grows a companion, `detect_secondary_langs`: a bounded
walk (depth ≤ 3 from the repo root) that records every directory owning a
recognized manifest. Bounded means bounded on every axis: configured
exclusions are honored; `.git` and vendored trees (`node_modules`,
`target`, `vendor`, `_build`, `deps`) are hard-skipped — a dependency's
manifest is not the repo speaking — and the walk stops at a fixed entry
budget, so a pathological tree costs a constant, not a crawl. The walk
uses `symlink_metadata`, never follows symlinks, and only joins relative
child names discovered beneath the repository root. It cannot escape the
repo through an absolute path, `..`, or a symlink.

Two suppressions keep the report honest instead of noisy. A nested
manifest of the *primary* kind is not a second language — a workspace
member (this repo's `xtask/Cargo.toml`) stays silent. And this repo also
shows the residual case: `fixtures/elixir_repo/mix.exs` would surface as
"Elixir (fixtures/elixir_repo)" unless fixture trees are in the configured
exclusions — that stays exclusion territory, named here so it is a
decision, not a surprise.

The strongest root claim remains the primary kind and sole source of the
validation plan. Secondaries are *reported*, not planned for:

```
## Languages
- primary: Rust (Cargo.toml at root)
- also present: Go (services/api), JS/TS (web)
```

Report-only is the honest first increment: it tells agents the repo's true
shape without pretending merged validation semantics that do not exist yet.
Per-subtree focused tiers and plan merging are future work on top of this
observation.

### Part C — `toimija languages`

A command that states the capability surface:

- the table: each language, its tier (full adapter / minimum pack), claiming
  manifests, and inferred commands;
- for the current repo: which kind claimed it, on what evidence, which
  commands were proven, which were dropped and why, and any secondaries;
- the precedence rule, stated: full adapters, then minimum packs in table
  order.

`docs/reference.md` gains the same table. Discoverability is the product
feature; the command is its interface.

## 4. Decisions

- **D9-1 — unproven commands are dropped, and the drop is noted.** A
  minimum-pack command is emitted only with manifest evidence; what was
  dropped and why lands in the plan's notes. No `[unproven]` tagging — a
  claim toimija cannot back does not ship, softened labels included.
- **D9-2 — proof is bounded file reads, never execution.** JSON parse of
  `package.json` (serde_json is already in-tree), section-aware scan of
  `pyproject.toml` (no `toml` crate, matching `rust.rs`). Never `npm run
  --list`, never `pytest --collect-only` — proving a command must not run
  anything.
- **D9-3 — secondary languages are observed, not planned.** Bounded walk:
  depth ≤ 3, a fixed entry budget, exclusions honored, vendored trees
  hard-skipped, symlinks never followed, primary-kind manifests
  suppressed. Primary kind and validation plan unchanged. Report-only
  until a real repo demands merged plans.
- **D9-4 — the capability surface is a stated fact.** `toimija languages`
  and `docs/reference.md` carry the same table; precedence is documented,
  not archaeological.

## 5. Non-goals

- No Tree-sitter, LSP, SCIP, or any source parsing — detection and proof
  stay at the manifest-and-path layer, permanently.
- No executing package managers or test runners to discover capabilities
  (D9-2).
- No merged validation plans or per-subtree focused tiers for monorepos —
  Part B observes; planning semantics are a later proposal.
- No full adapters for Go/Python/Node — the floor rises; the ceiling
  stays earned.
- No config-defined language packs or other plugin surface. A new minimum
  language remains one reviewed, compiled table row until the repo boundary
  changes explicitly.
- No user-global config file (`~/.config/toimija/…`) — every input
  toimija reads stays inside the repo, until a second machine proves the
  need.

## 6. Testing

- **Proof rules:** `package.json` fixtures — no scripts, `test` only,
  `lint`+`typecheck`, malformed JSON (malformed → drop with note, never
  panic); Python fixtures — `[tool.pytest.ini_options]`, root `pytest.ini`,
  generic `tests/`, `[tool.ruff]`, root `ruff.toml`, and `setup.py`
  without pytest evidence; Go unchanged.
- **Notes:** every dropped command produces exactly one note naming the
  command and the missing evidence.
- **Secondary walk:** monorepo fixture (`services/api/go.mod`,
  `web/package.json`, root `Cargo.toml`) → primary Rust, two secondaries
  with correct roots; exclusion fixture proves excluded dirs stay unseen;
  depth fixture proves the bound; a symlink to a manifest tree outside the
  repo stays unseen; `node_modules/<pkg>/package.json` stays unseen (hard
  skip); a nested `Cargo.toml` under a Rust primary stays silent
  (primary-kind suppression).
- **`toimija languages`:** snapshot on this repo (Rust, full adapter,
  proven commands) and on a fixture with drops and secondaries.
