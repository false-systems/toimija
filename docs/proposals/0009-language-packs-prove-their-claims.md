# Proposal 0009 — Language packs prove what they claim

**Status:** DRAFT (awaiting Yair)

**Date:** 2026-07-21

**Builds on:** the adapter architecture (`src/adapters/`), the minimum
language packs (`adapters/minimum.rs` — "a new language is data, not a
module"), and the trust model (#151: toimija reports what it verified, not
what it assumes). No boundary edits — no Tree-sitter, no LSP, no source
parsing, no network, no plugin code path. Everything below is manifest reads
and path checks, the same evidence class detection already uses.

---

## 1. The weak spots, named

The language layer is honest everywhere except its edges. Four gaps:

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
4. **A new language requires recompiling.** `MinimumLang` is already pure
   data, but the table is compiled in. The design's own logic — "a new
   language here is data, not a module" — stops one step short of letting
   the *user* supply the data.

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

- **Node:** parse `package.json` (serde_json). `npm test` requires a
  `scripts.test` entry; the strict tier prefers the scripts that actually
  exist (`lint`, then `typecheck`), emitting `npm run <name>` for each.
- **Python:** section-aware scan of `pyproject.toml`. `pytest` requires a
  `[tool.pytest]`/`[tool.pytest.ini_options]` table or a `tests/`
  directory; `ruff check .` requires a `[tool.ruff]` table. A `setup.py`
  repo with a `tests/` directory keeps `pytest`.
- **Go:** `go test ./...` and `go vet ./...` are defined by the toolchain,
  not the manifest — `go.mod` alone already proves them. No change.

A dropped command leaves a note in the plan, in the existing Notes tier:
`no test script in package.json — fast tier not inferred`. The plan's
`source` semantics are unchanged; `repo.yaml` validation config still
overrides everything inferred.

### Part B — secondary languages, observed and reported

`detect_repo_kind` grows a companion, `detect_secondary_langs`: a bounded
walk (depth ≤ 3 from the repo root, honoring configured exclusions and
skipping `.git`) that records every directory owning a recognized manifest.
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

- the table: each language, its tier (full adapter / minimum pack /
  config pack), claiming manifests, and inferred commands;
- for the current repo: which kind claimed it, on what evidence, which
  commands were proven, which were dropped and why, and any secondaries;
- the precedence rule, stated: full adapters, then packs in table order,
  config packs last unless overriding a builtin row.

`docs/reference.md` gains the same table. Discoverability is the product
feature; the command is its interface.

### Part D — config-defined language packs

`repo.yaml` may contribute rows with exactly the `MinimumLang` shape:

```yaml
languages:
  - kind: ruby
    name: Ruby
    manifests: [Gemfile]
    fast: ["bundle exec rspec"]
    strict: ["bundle exec rubocop"]
    extensions: [rb]
    companions: ["spec/{stem}_spec.rb"]
    test_markers: ["*_spec.rb"]
```

Config rows join the same engine — same detection, same existence-filtered
companions, same proof rules where they apply. A config row naming a
builtin kind overrides that row; full adapters (Rust, Elixir) outrank
config rows always. This is the plugin system we can have *without* a
plugin system: data through the config channel that already exists, no
code path, no loading, nothing executable.

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
- **D9-3 — secondary languages are observed, not planned.** Bounded walk,
  depth ≤ 3, exclusions honored; primary kind and validation plan
  unchanged. Report-only until a real repo demands merged plans.
- **D9-4 — language packs are data end-to-end.** `repo.yaml` rows with the
  `MinimumLang` shape; builtin full adapters outrank config; config
  overrides builtin *rows* by kind. No plugin code path.
- **D9-5 — the capability surface is a stated fact.** `toimija languages`
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
- No user-global config file (`~/.config/toimija/…`) — repo.yaml only,
  until a second machine proves the need.

## 6. Testing

- **Proof rules:** `package.json` fixtures — no scripts, `test` only,
  `lint`+`typecheck`, malformed JSON (malformed → drop with note, never
  panic); `pyproject.toml` fixtures — `[tool.pytest]` present/absent,
  `[tool.ruff]` present/absent, `setup.py` + `tests/`; Go unchanged.
- **Notes:** every dropped command produces exactly one note naming the
  command and the missing evidence.
- **Secondary walk:** monorepo fixture (`services/api/go.mod`,
  `web/package.json`, root `Cargo.toml`) → primary Rust, two secondaries
  with correct roots; exclusion fixture proves excluded dirs stay unseen;
  depth fixture proves the bound.
- **Config packs:** a `languages:` row detects, plans, and hints like a
  builtin; an overriding row beats the builtin row; a row naming `rust`
  loses to the full adapter.
- **`toimija languages`:** snapshot on this repo (Rust, full adapter,
  proven commands) and on a fixture with drops and secondaries.
