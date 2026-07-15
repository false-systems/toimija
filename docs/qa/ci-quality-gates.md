# CI quality gates

These are the proposed jobs; this QA branch does not silently broaden the
existing workflow without agreeing on support and runtime budgets.

| Job | Trigger | Commands/purpose | Duration | Artifacts | Failure policy |
|---|---|---|---|---|---|
| rust-core | every PR | fmt, clippy, unit/component/CLI tests | short | test log | required |
| qa-p0 | every PR | cargo test --test qa_p0 and state/JSON regressions | short | JSON fixtures/logs | required |
| qa-corpus | every PR | bounded parser corpus/property cases | short | failing seed | required once the corpus exists (P3-02); not yet wired |
| platform-macos | PR/main | release smoke + core tests | medium | binary/logs | required if supported |
| platform-linux | PR/main | release smoke + core tests | medium | binary/logs | required |
| platform-windows | scheduled/RC | compatibility smoke if supported | medium | binary/logs | required before claiming support |
| qa-scale | main/scheduled | generated profiles and p95/resource checks | long | benchmark JSON | warn until baselines exist; required at RC |
| qa-fuzz | scheduled | extended fuzz/corpus duration | long | crashers/corpus | required at RC |
| qa-package | RC/tag | clean-machine archive/Cargo install smoke | medium | archives/checksums | required |
| qa-upgrade | RC | prior generated-state/config fixtures | medium | compatibility report | required |

Do not add infinite retries. A flaky job is triaged, quarantined with an owner
and issue, or fixed; it is never made green by hiding the failure.
