//! The epistemic axis: knowledge the factory produces, not prose in a chat.
//!
//! The execution ontology says what happened. This says what was *established*,
//! what was *believed and disproved*, and what remains unproven — so the next
//! agent does not start from zero and cannot declare depth by assertion.
//!
//! Depth is not a personality trait of a worker. It is a required shape of work:
//! a [`WorkCharacter`] implies [`AssuranceObligation`]s, and an
//! [`AssuranceCase`] either discharges them with referenced evidence or does not.

use serde::{Deserialize, Serialize};

/// What is known about a claim. `Unverified` is the default and is never a
/// synonym for false; `Inconclusive` is never a synonym for either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// Stated, not yet examined.
    Unverified,
    /// Evidence supports it.
    Supported,
    /// Evidence contradicts it. **Retained, never deleted** — which beliefs were
    /// invalidated is part of what the work produced.
    Disproved,
    /// Examined; the evidence does not settle it.
    Inconclusive,
}

/// The epistemic role a claim plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// Proposed, to be tested.
    Hypothesis,
    /// Established by evidence against the system as it is.
    Finding,
    /// Relied upon without having been established. The dangerous one: an
    /// assumption is a hypothesis that skipped being tested.
    Assumption,
    /// A property that must hold. Violating one is what makes a defect a defect.
    Invariant,
}

/// One epistemic claim produced by work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub kind: ClaimKind,
    pub status: ClaimStatus,
    /// What is claimed, in one sentence.
    pub statement: String,
    /// Digests of artifacts supporting or contradicting it. A claim with
    /// `Supported` and no evidence is a claim asserting itself.
    #[serde(default)]
    pub evidence: Vec<String>,
}

impl Claim {
    /// Whether this claim is entitled to its status.
    ///
    /// `Supported` and `Disproved` are conclusions and require evidence.
    /// `Unverified` and `Inconclusive` are honest without any.
    pub fn is_entitled_to_its_status(&self) -> bool {
        match self.status {
            ClaimStatus::Supported | ClaimStatus::Disproved => !self.evidence.is_empty(),
            ClaimStatus::Unverified | ClaimStatus::Inconclusive => true,
        }
    }
}

/// How claims relate. The relations are what make an investigation more than a
/// list of observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// symptom → mechanism
    CausedBy,
    /// mechanism → abstraction that owns it
    OwnedBy,
    /// abstraction → invariant that constrains it
    ConstrainedBy,
    /// finding → assumption it invalidates
    Contradicts,
    /// finding → other surfaces the same mechanism reaches
    GeneralizesTo,
}

/// A directed edge between two claims, by index into [`Investigation::claims`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: usize,
    pub relation: Relation,
    pub to: usize,
}

/// The knowledge a piece of work produced.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Investigation {
    #[serde(default)]
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Investigation {
    /// Claims of a kind with a given status.
    pub fn of(&self, kind: ClaimKind, status: ClaimStatus) -> Vec<&Claim> {
        self.claims
            .iter()
            .filter(|claim| claim.kind == kind && claim.status == status)
            .collect()
    }

    /// Whether a root cause was established: a supported finding that is
    /// `CausedBy`-linked, and a violated invariant it reaches.
    ///
    /// A finding with no mechanism behind it is an observation, not a cause.
    pub fn root_cause_established(&self) -> bool {
        self.edges.iter().any(|edge| {
            edge.relation == Relation::CausedBy
                && edge.from != edge.to
                && self
                    .claims
                    .get(edge.to)
                    .is_some_and(|claim| !claim.statement.trim().is_empty())
                && self.claims.get(edge.from).is_some_and(|claim| {
                    claim.kind == ClaimKind::Finding && claim.status == ClaimStatus::Supported
                })
        })
    }

    /// Assumptions this investigation disproved. Preserved deliberately: the
    /// beliefs a piece of work invalidated are among its most reusable output.
    pub fn disproved_assumptions(&self) -> Vec<&Claim> {
        self.of(ClaimKind::Assumption, ClaimStatus::Disproved)
    }

    /// Every edge must point at claims that exist.
    pub fn edges_are_well_formed(&self) -> bool {
        self.edges
            .iter()
            .all(|edge| edge.from < self.claims.len() && edge.to < self.claims.len())
    }
}

/// Evidence that a previously possible undesirable behaviour is now prevented,
/// while preserving enough information to reproduce the old failure semantics.
///
/// **A test is not a regression witness.** A test proves behaviour. A witness
/// carries a relationship to an actual finding and the mechanism it exercises.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionWitness {
    /// The finding this witnesses, by digest.
    pub witnesses: String,
    /// The failure mechanism it exercises.
    pub exercises: String,
    /// The change it was verified against, by digest.
    pub verified_against: String,
    /// Identifier of the test that is the witness.
    pub test: String,
}

/// What kind of change the work is. Small on purpose: this is not a taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkCharacter {
    /// The question has not been answered. Carries one obligation that only
    /// reclassification can discharge, so absence blocks loudly rather than
    /// making the machinery inert.
    Unclassified,
    DefectRepair,
    BehaviorChange,
    ContractChange,
    ArchitecturalChange,
    Refactor,
}

impl Default for WorkCharacter {
    /// `Unclassified`, so a record written before the field existed loads as
    /// unclassified — which is true — rather than silently acquiring a bar.
    fn default() -> Self {
        Self::Unclassified
    }
}

/// A kind of knowledge that must be established before work may close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceObligation {
    /// The work's character must be declared. Nothing else can discharge this:
    /// the bar is unknown until the kind of change is stated.
    WorkClassified,
    RootCauseEstablished,
    InvariantDefined,
    RelatedSurfaceSearch,
    RegressionWitnessed,
    Verification,
    AdversarialVerification,
    ConsumerImpactAnalysis,
    HistoricalCompatibilityAnalysis,
    CrossImplementationConformance,
    AuthorityBoundaryReviewed,
    FailureModelReviewed,
}

impl WorkCharacter {
    /// What this character of work must establish before it can close.
    ///
    /// Structural, not advisory. The workflow does not ask a worker to be
    /// diligent; it reports which artifact is absent.
    pub fn required(self) -> &'static [AssuranceObligation] {
        use AssuranceObligation::*;
        match self {
            Self::Unclassified => &[WorkClassified],
            Self::Refactor => &[Verification, RegressionWitnessed],
            Self::DefectRepair => &[
                RootCauseEstablished,
                RelatedSurfaceSearch,
                RegressionWitnessed,
                Verification,
            ],
            Self::BehaviorChange => &[InvariantDefined, RegressionWitnessed, Verification],
            Self::ContractChange => &[
                RootCauseEstablished,
                InvariantDefined,
                RelatedSurfaceSearch,
                RegressionWitnessed,
                Verification,
                AdversarialVerification,
                ConsumerImpactAnalysis,
                HistoricalCompatibilityAnalysis,
                CrossImplementationConformance,
            ],
            Self::ArchitecturalChange => &[
                RootCauseEstablished,
                InvariantDefined,
                RelatedSurfaceSearch,
                RegressionWitnessed,
                Verification,
                AdversarialVerification,
                AuthorityBoundaryReviewed,
                FailureModelReviewed,
            ],
        }
    }
}

/// One claimed discharge of an assurance obligation.
///
/// **An assurance obligation is never discharged by declaration alone.** The
/// evidence references are what a judge checks; this structure only records
/// which artifacts are being offered for which obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssuranceDischarge {
    pub obligation: AssuranceObligation,
    /// Digests of the artifacts offered. Empty, or containing an empty string,
    /// discharges nothing.
    #[serde(default)]
    pub evidence: Vec<String>,
}

impl AssuranceDischarge {
    /// Whether anything was actually offered. Judging whether the offered
    /// artifacts *establish* the obligation is a semantic question and belongs
    /// to the assurance judge, not here.
    pub fn is_evidence_backed(&self) -> bool {
        !self.evidence.is_empty() && self.evidence.iter().all(|digest| !digest.trim().is_empty())
    }
}

/// One recorded change of a work's character.
///
/// Reclassification is permitted and **never silent**. Forbidding it would make
/// an early misclassification unfixable without abandoning the work; allowing it
/// unrecorded would reopen the bar-lowering attack upstream of [`WorkBar`].
/// Recorded, it is the `abandon --why` shape: legal, loud, permanent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reclassification {
    pub from: WorkCharacter,
    pub to: WorkCharacter,
    /// Why. Empty is not a reason.
    pub reason: String,
    /// Who decided. Empty is not an actor.
    pub actor: String,
    /// RFC 3339 UTC.
    pub at: String,
}

impl Reclassification {
    /// Whether every obligation of the new character was already required by the
    /// old one, and the old one required more.
    ///
    /// Compared by obligation set rather than by count: a character that swaps
    /// one obligation for another has not lowered the bar, and a count would say
    /// it had.
    pub fn lowers_the_bar(&self) -> bool {
        let before = self.from.required();
        let after = self.to.required();
        after.iter().all(|o| before.contains(o)) && after.len() < before.len()
    }

    /// A reclassification with no reason or no actor records nothing.
    pub fn is_audited(&self) -> bool {
        !self.reason.trim().is_empty() && !self.actor.trim().is_empty()
    }
}

/// The append-only record of a work's classification.
///
/// Chained the way Sykli's journal is: each change must begin where the previous
/// one ended, so an entry cannot be inserted, reordered or removed without
/// breaking the chain. The current character is the last link, never a field
/// somebody can edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationHistory {
    /// The character at ratification.
    pub initial: WorkCharacter,
    /// Every change since, in order. Append-only.
    #[serde(default)]
    pub changes: Vec<Reclassification>,
}

impl ClassificationHistory {
    /// A history that has never been reclassified.
    pub fn ratified_as(initial: WorkCharacter) -> Self {
        Self {
            initial,
            changes: Vec::new(),
        }
    }

    /// The character now in force: the last link, not an editable field.
    pub fn current(&self) -> WorkCharacter {
        self.changes.last().map_or(self.initial, |change| change.to)
    }

    /// Whether the chain is contiguous and every change is audited.
    ///
    /// A gap means an entry was inserted, reordered or removed.
    pub fn is_well_formed(&self) -> bool {
        let mut expected = self.initial;
        for change in &self.changes {
            if change.from != expected || !change.is_audited() {
                return false;
            }
            expected = change.to;
        }
        true
    }

    /// Whether the bar was ever lowered. A judge may treat that differently from
    /// a raise; it is never hidden either way.
    pub fn was_lowered(&self) -> bool {
        self.changes.iter().any(Reclassification::lowers_the_bar)
    }
}

impl Default for ClassificationHistory {
    /// Unclassified, with no changes: a record written before classification
    /// existed has genuinely never been classified.
    fn default() -> Self {
        Self::ratified_as(WorkCharacter::Unclassified)
    }
}

/// Where an assurance bar came from.
///
/// The bar is a property of the **ratified work**, not of the case assembled
/// afterwards. Without this distinction an assembler could write `Refactor` over
/// work Teko ratified as `ContractChange` and drop the bar from nine obligations
/// to two — grading its own work at a bar it chose after the fact, which is the
/// defect Kelpo's rigor echo already prevents for test rigor.
///
/// **Teko derives the bar. Kelpo decides whether it was met.** This type is what
/// keeps those from collapsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "bar", rename_all = "snake_case")]
pub enum WorkBar {
    /// No character declared. The first obligation is to answer the question.
    #[default]
    Unknown,
    /// Asserted by whoever assembled the case. Useful for computing what is
    /// still missing; **never** authoritative for sufficiency, because the
    /// assembler is the party the bar constrains.
    Asserted { character: WorkCharacter },
    /// Read from ratified work, named by its semantic id. Authoritative.
    Ratified {
        character: WorkCharacter,
        /// The semantic work id the bar was ratified for — never an artifact
        /// digest, which changes as the tree moves.
        work: String,
    },
}

impl WorkBar {
    /// The character, however it was obtained.
    pub fn character(&self) -> Option<WorkCharacter> {
        match self {
            Self::Unknown => None,
            Self::Asserted { character } | Self::Ratified { character, .. } => Some(*character),
        }
    }

    /// Only a bar read from ratified work may settle sufficiency.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, Self::Ratified { .. })
    }

    /// Whether this bar belongs to the work the case is about. An authoritative
    /// bar naming different work is not authoritative for *this* case.
    pub fn belongs_to(&self, work: &str) -> bool {
        match self {
            Self::Ratified { work: source, .. } => source == work,
            Self::Unknown | Self::Asserted { .. } => false,
        }
    }
}

/// The structured body of evidence supporting the claim that work satisfies its
/// obligations.
///
/// Kelpo judges an assurance case. Kelpo does not create one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssuranceCase {
    /// The **semantic work id**, not an artifact digest.
    ///
    /// `teko packet --artifact` re-renders against the current repository point,
    /// so its digest changes whenever the tree moves. Binding the bar to that
    /// would make the assurance bar go stale because a file changed — but the
    /// bar is a property of what the work *is*, and work identity survives every
    /// attempt, retry and resume.
    pub work: String,
    /// The assurance bar, and where it came from.
    #[serde(default)]
    pub bar: WorkBar,
    #[serde(default)]
    pub investigation: Investigation,
    #[serde(default)]
    pub regression_witnesses: Vec<RegressionWitness>,
    /// Obligations claimed as discharged, each with the evidence that discharges
    /// it. A discharge carrying no evidence discharges nothing — otherwise this
    /// field would be a declaration certifying itself, which is the defect this
    /// whole ontology exists to prevent.
    #[serde(default)]
    pub discharged: Vec<AssuranceDischarge>,
    /// Properties this work did not establish. Stating them is part of the work.
    #[serde(default)]
    pub unresolved: Vec<String>,
}

impl AssuranceCase {
    /// Obligations this case has evidence for.
    fn satisfied(&self) -> Vec<AssuranceObligation> {
        let mut satisfied: Vec<_> = self
            .discharged
            .iter()
            .filter(|discharge| discharge.is_evidence_backed())
            .map(|discharge| discharge.obligation)
            .collect();
        if self.investigation.root_cause_established() {
            satisfied.push(AssuranceObligation::RootCauseEstablished);
        }
        if !self
            .investigation
            .of(ClaimKind::Invariant, ClaimStatus::Supported)
            .is_empty()
        {
            satisfied.push(AssuranceObligation::InvariantDefined);
        }
        if !self.regression_witnesses.is_empty() {
            satisfied.push(AssuranceObligation::RegressionWitnessed);
        }
        satisfied.sort_unstable();
        satisfied.dedup();
        satisfied
    }

    /// What is still missing, in order. This is what Ohjaa derives a next action
    /// from and what Kelpo reports instead of a bare refusal.
    pub fn missing(&self) -> Vec<AssuranceObligation> {
        // An absent character is treated as `Unclassified`, whose single
        // obligation is to answer the question. Absence blocks loudly instead of
        // leaving the machinery quiet.
        let character = self.bar.character().unwrap_or(WorkCharacter::Unclassified);
        let satisfied = self.satisfied();
        character
            .required()
            .iter()
            .copied()
            .filter(|obligation| !satisfied.contains(obligation))
            .collect()
    }

    /// Sufficient only when the bar is authoritative, belongs to this work, is
    /// classified, and nothing is missing.
    ///
    /// An asserted bar never suffices however complete the evidence: the party
    /// assembling the case is the party the bar exists to constrain.
    pub fn is_sufficient(&self) -> bool {
        self.bar.is_authoritative()
            && self.bar.belongs_to(&self.work)
            && self.bar.character() != Some(WorkCharacter::Unclassified)
            && self.missing().is_empty()
    }

    /// Why this case cannot settle, when it cannot. `None` means it can.
    pub fn blocking_reason(&self) -> Option<&'static str> {
        if !self.bar.is_authoritative() {
            return Some("the assurance bar was asserted, not read from ratified work");
        }
        if !self.bar.belongs_to(&self.work) {
            return Some("the assurance bar belongs to different work");
        }
        if self.bar.character() == Some(WorkCharacter::Unclassified) {
            return Some("the work has not been classified");
        }
        (!self.missing().is_empty()).then_some("assurance obligations remain undischarged")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: &str = "sha256:work";

    fn ratified(character: WorkCharacter) -> WorkBar {
        WorkBar::Ratified {
            character,
            work: WORK.into(),
        }
    }

    fn finding(statement: &str, evidence: &str) -> Claim {
        Claim {
            kind: ClaimKind::Finding,
            status: ClaimStatus::Supported,
            statement: statement.into(),
            evidence: vec![evidence.into()],
        }
    }

    #[test]
    fn a_conclusion_without_evidence_is_not_entitled_to_its_status() {
        let asserted = Claim {
            kind: ClaimKind::Finding,
            status: ClaimStatus::Supported,
            statement: "it works".into(),
            evidence: vec![],
        };
        assert!(!asserted.is_entitled_to_its_status());

        let honest = Claim {
            status: ClaimStatus::Inconclusive,
            ..asserted.clone()
        };
        assert!(honest.is_entitled_to_its_status());
        assert!(finding("x", "sha256:e").is_entitled_to_its_status());
    }

    #[test]
    fn an_observation_without_a_mechanism_is_not_a_root_cause() {
        let mut investigation = Investigation {
            claims: vec![finding(
                "kelpo attaches context after production",
                "sha256:a",
            )],
            edges: vec![],
        };
        assert!(!investigation.root_cause_established());

        investigation.claims.push(Claim {
            kind: ClaimKind::Invariant,
            status: ClaimStatus::Supported,
            statement: "evidence provenance must be intrinsic at production time".into(),
            evidence: vec!["sha256:b".into()],
        });
        investigation.edges.push(Edge {
            from: 0,
            relation: Relation::CausedBy,
            to: 1,
        });
        assert!(investigation.root_cause_established());
        assert!(investigation.edges_are_well_formed());
        for (from, to) in [(0, 999), (999, 1), (0, 0)] {
            investigation.edges[0].from = from;
            investigation.edges[0].to = to;
            assert!(!investigation.root_cause_established());
        }
        investigation.edges[0].from = 0;
        investigation.edges[0].to = 1;
        investigation.claims[1].statement.clear();
        assert!(!investigation.root_cause_established());
    }

    /// The hostile-producer investigation, as the ontology would have carried it.
    #[test]
    fn disproved_assumptions_are_retained_as_output() {
        let investigation = Investigation {
            claims: vec![
                Claim {
                    kind: ClaimKind::Assumption,
                    status: ClaimStatus::Disproved,
                    statement: "session disjointness establishes producer independence".into(),
                    evidence: vec!["sha256:kelpo-352".into()],
                },
                finding("session ids are self-mintable", "sha256:session-534"),
            ],
            edges: vec![Edge {
                from: 1,
                relation: Relation::Contradicts,
                to: 0,
            }],
        };
        let disproved = investigation.disproved_assumptions();
        assert_eq!(disproved.len(), 1);
        assert!(disproved[0].statement.contains("producer independence"));
        assert!(investigation.edges_are_well_formed());
    }

    /// INVARIANT: the work character determines the minimum assurance bar
    /// before closure — and an unanswered character is itself the first bar.
    #[test]
    fn unclassified_work_blocks_loudly_rather_than_quietly() {
        for bar in [WorkBar::Unknown, ratified(WorkCharacter::Unclassified)] {
            let case = AssuranceCase {
                work: WORK.into(),
                bar,
                ..Default::default()
            };
            assert!(!case.is_sufficient());
            assert_eq!(case.missing(), vec![AssuranceObligation::WorkClassified]);
        }
    }

    /// `WorkClassified` is not dischargeable by evidence. Only answering the
    /// question answers the question.
    #[test]
    fn classification_cannot_be_bought_with_evidence() {
        let case = AssuranceCase {
            work: "sha256:work".into(),
            bar: ratified(WorkCharacter::Unclassified),
            discharged: vec![AssuranceDischarge {
                obligation: AssuranceObligation::WorkClassified,
                evidence: vec!["sha256:a-document-claiming-this-is-a-refactor".into()],
            }],
            ..Default::default()
        };
        assert!(!case.is_sufficient());
    }

    /// INVARIANT: an assurance obligation is never discharged by declaration
    /// alone.
    #[test]
    fn a_discharge_without_evidence_discharges_nothing() {
        let empty = AssuranceDischarge {
            obligation: AssuranceObligation::Verification,
            evidence: vec![],
        };
        let blank = AssuranceDischarge {
            obligation: AssuranceObligation::Verification,
            evidence: vec!["   ".into()],
        };
        let real = AssuranceDischarge {
            obligation: AssuranceObligation::Verification,
            evidence: vec!["sha256:receipt".into()],
        };
        assert!(!empty.is_evidence_backed());
        assert!(!blank.is_evidence_backed());
        assert!(real.is_evidence_backed());

        let case = AssuranceCase {
            work: WORK.into(),
            bar: ratified(WorkCharacter::Refactor),
            discharged: vec![
                empty,
                AssuranceDischarge {
                    obligation: AssuranceObligation::RegressionWitnessed,
                    evidence: vec![String::new()],
                },
            ],
            ..Default::default()
        };
        assert_eq!(case.missing().len(), 2);
    }

    /// INVARIANT: a successful implementation with an incomplete assurance case
    /// is not closable work. Nothing here knows or cares that the change works.
    #[test]
    fn a_working_change_with_an_incomplete_case_is_not_sufficient() {
        let case = AssuranceCase {
            work: WORK.into(),
            bar: ratified(WorkCharacter::ContractChange),
            discharged: vec![AssuranceDischarge {
                obligation: AssuranceObligation::Verification,
                evidence: vec!["sha256:all-tests-green".into()],
            }],
            ..Default::default()
        };
        assert!(!case.is_sufficient());
        assert!(case.missing().len() > 5);
    }

    /// The tree-identity fix, judged as what it actually was.
    #[test]
    fn a_contract_change_is_blocked_until_its_obligations_are_discharged() {
        use AssuranceObligation::*;
        let mut case = AssuranceCase {
            work: "sha256:tree-identity".into(),
            bar: WorkBar::Ratified {
                character: WorkCharacter::ContractChange,
                work: "sha256:tree-identity".into(),
            },
            investigation: Investigation {
                claims: vec![
                    finding(
                        "toimija and sykli disagree on config membership",
                        "sha256:snapshot-55",
                    ),
                    Claim {
                        kind: ClaimKind::Invariant,
                        status: ClaimStatus::Supported,
                        statement: "tree identity has one meaning everywhere".into(),
                        evidence: vec!["sha256:faija-terms".into()],
                    },
                ],
                edges: vec![Edge {
                    from: 0,
                    relation: Relation::CausedBy,
                    to: 1,
                }],
            },
            regression_witnesses: vec![RegressionWitness {
                witnesses: "sha256:snapshot-55".into(),
                exercises: "differing namespace exclusion sets".into(),
                verified_against: "sha256:faija-terms".into(),
                test: "t4_legacy_implementations_disagree_on_config_membership".into(),
            }],
            ..Default::default()
        };

        let missing = case.missing();
        assert!(!case.is_sufficient());
        assert!(missing.contains(&AdversarialVerification));
        assert!(missing.contains(&ConsumerImpactAnalysis));
        assert!(missing.contains(&CrossImplementationConformance));
        // Already established by the investigation and the witness:
        assert!(!missing.contains(&RootCauseEstablished));
        assert!(!missing.contains(&InvariantDefined));
        assert!(!missing.contains(&RegressionWitnessed));

        for obligation in missing {
            case.discharged.push(AssuranceDischarge {
                obligation,
                evidence: vec!["sha256:later".into()],
            });
        }
        assert!(case.is_sufficient());
    }

    /// The bar-lowering attack: assemble the case declaring `Refactor` over work
    /// Teko ratified as `ContractChange`, dropping nine obligations to two.
    #[test]
    fn an_asserted_bar_never_settles_sufficiency() {
        let discharged = |obligation| AssuranceDischarge {
            obligation,
            evidence: vec!["sha256:e".into()],
        };
        let case = AssuranceCase {
            work: WORK.into(),
            bar: WorkBar::Asserted {
                character: WorkCharacter::Refactor,
            },
            discharged: vec![
                discharged(AssuranceObligation::Verification),
                discharged(AssuranceObligation::RegressionWitnessed),
            ],
            ..Default::default()
        };
        // Every obligation of the asserted character is discharged...
        assert!(case.missing().is_empty());
        // ...and it still does not settle, because the assembler chose the bar.
        assert!(!case.is_sufficient());
        assert_eq!(
            case.blocking_reason(),
            Some("the assurance bar was asserted, not read from ratified work")
        );
    }

    /// A bar read from ratified work, but somebody else's ratified work.
    #[test]
    fn a_bar_from_other_work_does_not_settle_this_case() {
        let case = AssuranceCase {
            work: WORK.into(),
            bar: WorkBar::Ratified {
                character: WorkCharacter::Refactor,
                work: "sha256:some-other-easier-work".into(),
            },
            discharged: vec![
                AssuranceDischarge {
                    obligation: AssuranceObligation::Verification,
                    evidence: vec!["sha256:e".into()],
                },
                AssuranceDischarge {
                    obligation: AssuranceObligation::RegressionWitnessed,
                    evidence: vec!["sha256:e".into()],
                },
            ],
            ..Default::default()
        };
        assert!(!case.is_sufficient());
        assert_eq!(
            case.blocking_reason(),
            Some("the assurance bar belongs to different work")
        );
    }

    fn change(from: WorkCharacter, to: WorkCharacter) -> Reclassification {
        Reclassification {
            from,
            to,
            reason: "scope turned out to cross a contract".into(),
            actor: "yair".into(),
            at: "2026-09-17T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_current_character_is_the_last_link_not_a_field() {
        let mut history = ClassificationHistory::ratified_as(WorkCharacter::Unclassified);
        assert_eq!(history.current(), WorkCharacter::Unclassified);
        history.changes.push(change(
            WorkCharacter::Unclassified,
            WorkCharacter::DefectRepair,
        ));
        history.changes.push(change(
            WorkCharacter::DefectRepair,
            WorkCharacter::ContractChange,
        ));
        assert_eq!(history.current(), WorkCharacter::ContractChange);
        assert!(history.is_well_formed());
        assert!(!history.was_lowered());
    }

    /// Append-only is checkable: a gap means an entry was inserted, reordered or
    /// removed. Same shape as Sykli's journal chain.
    #[test]
    fn a_broken_chain_is_not_well_formed() {
        let history = ClassificationHistory {
            initial: WorkCharacter::ContractChange,
            // Claims to start from Refactor, but ratification said ContractChange.
            changes: vec![change(WorkCharacter::Refactor, WorkCharacter::Refactor)],
        };
        assert!(!history.is_well_formed());
    }

    #[test]
    fn an_unaudited_reclassification_is_not_well_formed() {
        for (reason, actor) in [("", "yair"), ("because", ""), ("  ", "  ")] {
            let history = ClassificationHistory {
                initial: WorkCharacter::ContractChange,
                changes: vec![Reclassification {
                    from: WorkCharacter::ContractChange,
                    to: WorkCharacter::Refactor,
                    reason: reason.into(),
                    actor: actor.into(),
                    at: "2026-09-17T00:00:00Z".into(),
                }],
            };
            assert!(!history.is_well_formed());
        }
    }

    /// Lowering is legal and permanently visible — the `abandon --why` shape.
    #[test]
    fn lowering_the_bar_is_recorded_not_prevented() {
        let history = ClassificationHistory {
            initial: WorkCharacter::ContractChange,
            changes: vec![change(
                WorkCharacter::ContractChange,
                WorkCharacter::Refactor,
            )],
        };
        assert!(history.is_well_formed());
        assert!(history.was_lowered());
        assert_eq!(history.current(), WorkCharacter::Refactor);
    }

    /// Compared by obligation set, not by count: swapping one obligation for
    /// another is not a lowering, and a count would say it was.
    #[test]
    fn a_sideways_reclassification_does_not_lower_the_bar() {
        // DefectRepair and BehaviorChange both require three, with different sets.
        let sideways = change(WorkCharacter::DefectRepair, WorkCharacter::BehaviorChange);
        assert!(!sideways.lowers_the_bar());
        assert!(change(WorkCharacter::ContractChange, WorkCharacter::Refactor).lowers_the_bar());
        assert!(!change(WorkCharacter::Refactor, WorkCharacter::ContractChange).lowers_the_bar());
    }

    #[test]
    fn heavier_work_requires_strictly_more() {
        assert_eq!(WorkCharacter::Unclassified.required().len(), 1);
        let refactor = WorkCharacter::Refactor.required().len();
        let defect = WorkCharacter::DefectRepair.required().len();
        let contract = WorkCharacter::ContractChange.required().len();
        assert!(refactor < defect);
        assert!(defect < contract);
    }
}

/// What a judge concluded about an assurance case.
///
/// Six states, never a boolean. `Inconclusive` is "come back with more
/// evidence"; `Insufficient` is "the bar is not met"; `Invalid` is "this
/// document is not a judgement of anything". Collapsing them would make every
/// consumer choose between tolerating uncertainty and rejecting honesty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceResult {
    /// Every required obligation passed structural checks; correctness remains unestablished.
    StructurallyAdequate,
    /// At least one required obligation is not discharged.
    Insufficient,
    /// The judge could not reach an answer. Never success.
    Inconclusive,
    /// The document does not judge what it claims to.
    Invalid,
    /// The judgement was made somewhere the consumer is not.
    Stale,
    /// The judge does not support this bar.
    Unsupported,
}

/// Whether an assurance verdict may resolve a consumer's assurance obligation.
///
/// The variants a verdict can fail on are kept apart deliberately: "judged for
/// other work", "judged under a weaker bar" and "judged at a tree that has
/// moved" are different failures calling for different responses, and a boolean
/// would erase all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceApplicability {
    /// Same work, same character, same required set, same case, current anchor,
    /// and the result is `StructurallyAdequate`; this does not establish correctness.
    Applicable,
    WrongWork,
    WrongCharacter,
    /// Same character, different required set — a bar that is not this bar.
    WrongBar,
    WrongCase,
    StaleTree,
    StaleRepositoryPoint,
    InvalidVerdict,
    Insufficient,
    Inconclusive,
    Unsupported,
}

impl AssuranceApplicability {
    /// Only `Applicable` resolves an obligation. Everything else leaves it
    /// unresolved, and says which way it failed.
    pub fn resolves(self) -> bool {
        matches!(self, Self::Applicable)
    }

    /// A short, stable reason for a refusal message.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Applicable => "applicable",
            Self::WrongWork => "the verdict judged different work",
            Self::WrongCharacter => "the verdict judged a different work character",
            Self::WrongBar => "the verdict judged a different set of obligations",
            Self::WrongCase => "the verdict judged a different assurance case",
            Self::StaleTree => "the verdict was made at a tree that has since moved",
            Self::StaleRepositoryPoint => {
                "the verdict was made at a repository point that has since changed"
            }
            Self::InvalidVerdict => "the verdict is not a valid judgement",
            Self::Insufficient => "the assurance case does not satisfy the bar",
            Self::Inconclusive => "the assurance case could not be judged",
            Self::Unsupported => "the judge does not support this bar",
        }
    }
}
