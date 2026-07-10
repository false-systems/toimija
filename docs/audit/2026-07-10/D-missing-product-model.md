# D. Missing product model

The minimum useful model is not 18 independent abstractions. Several requested
nouns should be fields or tagged records inside six aggregates. Add them only as
the corresponding P0/P1 behavior lands.

| Object | Why needed / current equivalent | Essential fields and relationships | Persistence | v1? |
|---|---|---|---|---|
| Repository | Stable identity and capabilities above one checkout. Current `RepoInfo` is descriptive config only. | identity (best local evidence), common Git dir, config, project roots, capabilities | Recomputed; config durable | Yes |
| Worktree | Separate checkout, gitdir/index/sparse state, and local dirt. Current `GitState.repo_root/git_dir` is insufficient. | repository ref, root, gitdir, common-dir, HEAD/index identity, sparse/bare/detached flags | Snapshot in packet/run | Yes |
| RepositoryState | One observed state with coverage. Current `GitState` + `Consistency` approximate it. | worktree, HEAD, index layers, untracked/ignored policy, operations, submodules, token, observed-at, stage results | Packet/run snapshots | Yes |
| BranchState | Distinguish graph facts from intent. Current branch/base/merge-base fields are scattered. | ref/OID, upstream OID, ahead/behind, base candidates, merge-base, committed surface, remote freshness | Recomputed | Yes |
| Task | State what the agent is meant to accomplish. No equivalent beyond free-text intent. | objective, supplied constraints, requested scope/exclusions, source refs, open questions | Run ledger; optionally explicit local task file | Yes |
| Intent | Evidence-backed interpretation, not a synonym for diff. Current session `intent` is one optional string. | claim, sources, confidence, contradictions, confirmation status | Run snapshot/event | Yes, as part of Task/BranchState |
| Instruction | Preserve policy scope and precedence. Current model lists doc paths. | source path/range/hash, scope root/glob, authority, precedence, text/reference, status, conflicts | Recomputed; source stays in repo | Yes |
| ChangedSurface | Preserve committed/index/worktree layers and provenance. Current packet partly does this. | path identity, layer(s), status, old path, submodule/index details, source observation | Packet/run snapshot | Yes |
| ContextItem | Unit of selection and explanation. No current equivalent; `ReadFirstEntry` is too narrow. | id, kind, source/evidence, relevance, mandatory, cost, content reference, sensitivity | Selection record; content usually not duplicated | Yes |
| ContextSelection | Make budget omissions reproducible. Current truncation is rendering-only. | item ids, budget unit/value, algorithm version, selected order, omissions/reasons, total cost | Packet/run | Yes |
| ValidationRequirement | State what proof is required. Current command strings conflate requirement and execution. | id, trigger/evidence, level, coverage, exclusions, required/optional, environment, safety, cost | Config/discovery + packet snapshot | Yes |
| ValidationPlan | Resolve requirements to runnable/manual checks. Current `ValidationPlan` is the closest existing object. | requirement refs, CommandSpec/manual step, cwd, dependencies, unsupported/blocked reasons, coverage summary | Packet/run snapshot | Yes; refactor current type |
| ValidationResult | Distinguish plan from evidence. Absent. | requirement id, status enum, timestamps, argv/cwd, exit/signal, bounded output refs/digests, tool/env fingerprint, state token | Append-only run event | Yes, minimal |
| Evidence | Link claims to reproducible observations. Sauma is an envelope, not the claim/evidence model. | id/digest, kind, producer, source refs, observed state token, freshness rule, redaction | Inline small refs; files/Git objects for payloads | Yes, small |
| AgentRun | Bounded attempt and recovery unit. Session marker is mutable presence, not a run record. | run id/parent, task, agent/tool versions, entry/end state, status, events, selection, results, handoff | Local append-only JSONL | Yes |
| Handoff | Successor-facing projection of a run. Current record is a current-state snapshot. | run/state refs, pre-existing vs attributable changes, assumptions, incomplete work, results/omissions, next step | Derived durable local record; optional share later | Yes |
| CapabilityCoverage | Prevent missing discovery from reading as “nothing found.” Absent. | capability, supported/degraded/unsupported, evidence source, reason, affected claims | Packet/run | Yes |
| Uncertainty | Make inference strength machine-readable. Warnings are prose today. | code, claim ref, level/confidence, reason, resolution action | Inline in packet/run | Yes, as a shared tagged value, not a service |

## Aggregate boundaries

1. `PreparationReport`: Repository, Worktree, RepositoryState, BranchState,
   Task, applicable Instructions, CapabilityCoverage, Uncertainty, and an overall
   `ready | ready_with_warnings | blocked` verdict.
2. `ContextSelection`: items selected from that report under a declared budget.
3. `ValidationPlan`: requirements and runnable/manual resolutions.
4. `AgentRun`: append-only lifecycle and evidence references.
5. `Handoff`: a projection from AgentRun plus current repository state.
6. `ApiV1` DTOs: explicit machine contracts, separate from internal structs.

Do not create repositories, traits, or persistence layers for each row. Plain
serde structs/enums in the existing crate are sufficient until behavior proves
otherwise.

