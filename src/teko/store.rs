use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{
    CompletionWitness, EvidenceSet, GateReceipt, ManualClaim, ObligationKind, Spec, SpecId,
    SpecStatus, Work, WorkId, WorkStatus,
};

pub const STORE_PATH: &str = ".toimija/teko.redb";
const SCHEMA_VERSION: u64 = 1;
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const SPECS: TableDefinition<&str, &[u8]> = TableDefinition::new("specs");
const WORK: TableDefinition<&str, &[u8]> = TableDefinition::new("work");
const STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("state");
const EVIDENCE: TableDefinition<&str, &[u8]> = TableDefinition::new("evidence");
const EVENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("events");
const ACTIVE_KEY: &str = "active_work";
const SCHEMA_KEY: &str = "schema_version";
const EVENT_SEQUENCE_KEY: &str = "event_sequence";

#[derive(Debug, Serialize, Deserialize)]
struct Stored<T> {
    schema: u64,
    value: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TekoEvent {
    pub sequence: u64,
    pub at: u64,
    pub kind: EventKind,
    pub spec_id: Option<SpecId>,
    pub work_id: Option<WorkId>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SpecCreated,
    SpecRatified,
    SpecSuperseded,
    WorkCreated,
    WorkActivated,
    WorkBlocked,
    ManualClaimed,
    GatePassed,
    GateFailed,
    GateInvalidated,
    ScopeViolated,
    WorkVerified,
    WorkClosed,
}

pub struct TekoStore {
    database: Database,
    path: PathBuf,
}

impl TekoStore {
    pub fn open(repo_root: &Path) -> Result<Self> {
        let path = repo_root.join(STORE_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let database = Database::create(&path)
            .with_context(|| format!("failed to open Teko store {}", path.display()))?;
        let store = Self { database, path };
        store.initialize()?;
        Ok(store)
    }

    pub fn open_existing(repo_root: &Path) -> Result<Option<Self>> {
        let path = repo_root.join(STORE_PATH);
        if !path.exists() {
            return Ok(None);
        }
        Self::open(repo_root).map(Some)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn initialize(&self) -> Result<()> {
        let transaction = self.database.begin_write()?;
        {
            let mut meta = transaction.open_table(META)?;
            let stored_schema = meta.get(SCHEMA_KEY)?.map(|version| version.value());
            match stored_schema {
                Some(version) if version != SCHEMA_VERSION => bail!(
                    "unsupported Teko store schema {}; this binary supports {}",
                    version,
                    SCHEMA_VERSION
                ),
                Some(_) => {}
                None => {
                    meta.insert(SCHEMA_KEY, SCHEMA_VERSION)?;
                    meta.insert(EVENT_SEQUENCE_KEY, 0)?;
                }
            }
            transaction.open_table(SPECS)?;
            transaction.open_table(WORK)?;
            transaction.open_table(STATE)?;
            transaction.open_table(EVIDENCE)?;
            transaction.open_table(EVENTS)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn create_spec(&self, spec: &Spec) -> Result<()> {
        let transaction = self.database.begin_write()?;
        {
            let mut specs = transaction.open_table(SPECS)?;
            if specs.get(spec.id.0.as_str())?.is_some() {
                bail!("spec `{}` already exists", spec.id);
            }
            specs.insert(spec.id.0.as_str(), encode(spec)?.as_slice())?;
        }
        append_event(
            &transaction,
            EventKind::SpecCreated,
            Some(&spec.id),
            None,
            None,
        )?;
        if spec.status == SpecStatus::Ratified {
            append_event(
                &transaction,
                EventKind::SpecRatified,
                Some(&spec.id),
                None,
                None,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn update_spec(&self, spec: &Spec) -> Result<()> {
        let transaction = self.database.begin_write()?;
        {
            let mut specs = transaction.open_table(SPECS)?;
            if specs.get(spec.id.0.as_str())?.is_none() {
                bail!("spec `{}` does not exist", spec.id);
            }
            specs.insert(spec.id.0.as_str(), encode(spec)?.as_slice())?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn get_spec(&self, id: &SpecId) -> Result<Option<Spec>> {
        let transaction = self.database.begin_read()?;
        let table = transaction.open_table(SPECS)?;
        table
            .get(id.0.as_str())?
            .map(|value| decode(value.value(), "spec"))
            .transpose()
    }

    pub fn list_specs(&self) -> Result<Vec<Spec>> {
        let transaction = self.database.begin_read()?;
        let table = transaction.open_table(SPECS)?;
        table
            .iter()?
            .map(|entry| {
                let (_, value) = entry?;
                decode(value.value(), "spec")
            })
            .collect()
    }

    /// Ratify if explicitly requested, create the work, append both lifecycle
    /// events, and publish the active pointer in one redb transaction.
    pub fn activate_work(&self, mut work: Work, ratify: bool) -> Result<Work> {
        let transaction = self.database.begin_write()?;
        let mut spec = {
            let specs = transaction.open_table(SPECS)?;
            let bytes = specs
                .get(work.spec_id.0.as_str())?
                .ok_or_else(|| anyhow::anyhow!("spec `{}` does not exist", work.spec_id))?;
            decode::<Spec>(bytes.value(), "spec")?
        };
        if spec.status == SpecStatus::Draft && ratify {
            spec.status = SpecStatus::Ratified;
            {
                let mut specs = transaction.open_table(SPECS)?;
                specs.insert(spec.id.0.as_str(), encode(&spec)?.as_slice())?;
            }
            append_event(
                &transaction,
                EventKind::SpecRatified,
                Some(&spec.id),
                None,
                None,
            )?;
        }
        if spec.status != SpecStatus::Ratified {
            bail!(
                "spec `{}` is {:?}; active work requires explicit ratification",
                spec.id,
                spec.status
            );
        }
        {
            let state = transaction.open_table(STATE)?;
            if let Some(active) = state.get(ACTIVE_KEY)? {
                let active: Stored<WorkId> = decode_stored(active.value(), "active work pointer")?;
                bail!(
                    "work `{}` is already active in this repository",
                    active.value
                );
            }
        }
        {
            let mut works = transaction.open_table(WORK)?;
            if works.get(work.id.0.as_str())?.is_some() {
                bail!("work `{}` already exists", work.id);
            }
            work.status = WorkStatus::Active;
            works.insert(work.id.0.as_str(), encode(&work)?.as_slice())?;
        }
        {
            let mut evidence = transaction.open_table(EVIDENCE)?;
            evidence.insert(
                work.id.0.as_str(),
                encode(&EvidenceSet::default())?.as_slice(),
            )?;
        }
        append_event(
            &transaction,
            EventKind::WorkCreated,
            Some(&spec.id),
            Some(&work.id),
            None,
        )?;
        append_event(
            &transaction,
            EventKind::WorkActivated,
            Some(&spec.id),
            Some(&work.id),
            Some(work.origin.tree_oid.0.clone()),
        )?;
        {
            let mut state = transaction.open_table(STATE)?;
            state.insert(ACTIVE_KEY, encode(&work.id)?.as_slice())?;
        }
        transaction.commit()?;
        Ok(work)
    }

    pub fn get_work(&self, id: &WorkId) -> Result<Option<Work>> {
        let transaction = self.database.begin_read()?;
        let table = transaction.open_table(WORK)?;
        table
            .get(id.0.as_str())?
            .map(|value| decode(value.value(), "work"))
            .transpose()
    }

    pub fn active_work(&self) -> Result<Option<Work>> {
        let transaction = self.database.begin_read()?;
        let state = transaction.open_table(STATE)?;
        let Some(pointer) = state.get(ACTIVE_KEY)? else {
            return Ok(None);
        };
        let pointer: WorkId = decode(pointer.value(), "active work pointer")?;
        let works = transaction.open_table(WORK)?;
        let bytes = works.get(pointer.0.as_str())?.ok_or_else(|| {
            anyhow::anyhow!(
                "Teko store is inconsistent: active pointer references missing work `{pointer}`"
            )
        })?;
        let work: Work = decode(bytes.value(), "active work")?;
        if !matches!(work.status, WorkStatus::Active | WorkStatus::Verified) {
            bail!(
                "Teko store is inconsistent: active work `{}` has status {:?}",
                work.id,
                work.status
            );
        }
        Ok(Some(work))
    }

    pub fn evidence(&self, work_id: &WorkId) -> Result<EvidenceSet> {
        let transaction = self.database.begin_read()?;
        let table = transaction.open_table(EVIDENCE)?;
        let bytes = table.get(work_id.0.as_str())?.ok_or_else(|| {
            anyhow::anyhow!("Teko store is inconsistent: missing evidence for `{work_id}`")
        })?;
        decode(bytes.value(), "evidence")
    }

    pub fn put_gate_receipt(&self, receipt: GateReceipt) -> Result<()> {
        let transaction = self.database.begin_write()?;
        let work = load_work(&transaction, &receipt.work_id)?;
        let spec = load_spec(&transaction, &work.spec_id)?;
        let gate = spec
            .verification
            .iter()
            .find(|gate| gate.id == receipt.gate_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "gate `{}` does not belong to work `{}`",
                    receipt.gate_id,
                    receipt.work_id
                )
            })?;
        if gate.command != receipt.command {
            bail!(
                "gate `{}` command does not match the ratified specification",
                receipt.gate_id
            );
        }
        let mut evidence = load_evidence(&transaction, &receipt.work_id)?;
        evidence.gate_receipts.push(receipt.clone());
        {
            let mut table = transaction.open_table(EVIDENCE)?;
            table.insert(receipt.work_id.0.as_str(), encode(&evidence)?.as_slice())?;
        }
        append_event(
            &transaction,
            if receipt.exit_status == 0 {
                EventKind::GatePassed
            } else {
                EventKind::GateFailed
            },
            Some(&work.spec_id),
            Some(&work.id),
            Some(format!("{} on {}", receipt.gate_id, receipt.tree_oid)),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn append_gate_invalidation(&self, work: &Work, gate: &str, reason: &str) -> Result<()> {
        let transaction = self.database.begin_write()?;
        append_event(
            &transaction,
            EventKind::GateInvalidated,
            Some(&work.spec_id),
            Some(&work.id),
            Some(format!("{gate}: {reason}")),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn put_manual_claim(&self, claim: ManualClaim) -> Result<()> {
        let transaction = self.database.begin_write()?;
        let work = load_work(&transaction, &claim.work_id)?;
        let valid = work.obligations.iter().any(|obligation| {
            obligation.id == claim.obligation_id
                && matches!(obligation.kind, ObligationKind::Manual)
        });
        if !valid {
            bail!(
                "manual obligation `{}` does not belong to work `{}`",
                claim.obligation_id,
                claim.work_id
            );
        }
        let mut evidence = load_evidence(&transaction, &claim.work_id)?;
        if !evidence.manual_claims.iter().any(|existing| {
            existing.work_id == claim.work_id && existing.obligation_id == claim.obligation_id
        }) {
            evidence.manual_claims.push(claim.clone());
            let mut table = transaction.open_table(EVIDENCE)?;
            table.insert(claim.work_id.0.as_str(), encode(&evidence)?.as_slice())?;
            append_event(
                &transaction,
                EventKind::ManualClaimed,
                Some(&work.spec_id),
                Some(&work.id),
                Some(claim.obligation_id.0),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_verified(&self, id: &WorkId) -> Result<()> {
        let transaction = self.database.begin_write()?;
        let mut work = load_work(&transaction, id)?;
        if work.status != WorkStatus::Verified {
            work.status = WorkStatus::Verified;
            {
                let mut works = transaction.open_table(WORK)?;
                works.insert(id.0.as_str(), encode(&work)?.as_slice())?;
            }
            append_event(
                &transaction,
                EventKind::WorkVerified,
                Some(&work.spec_id),
                Some(id),
                None,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Store the witness, close the work, implement the spec, append the close
    /// event, and clear the active pointer atomically.
    pub fn close_work(&self, witness: CompletionWitness) -> Result<()> {
        let transaction = self.database.begin_write()?;
        let mut work = load_work(&transaction, &witness.work_id)?;
        let active = {
            let state = transaction.open_table(STATE)?;
            state
                .get(ACTIVE_KEY)?
                .map(|value| decode::<WorkId>(value.value(), "active work pointer"))
                .transpose()?
        };
        if active.as_ref() != Some(&work.id) {
            bail!("work `{}` is not the active work", work.id);
        }
        if !matches!(work.status, WorkStatus::Active | WorkStatus::Verified) {
            bail!("work `{}` cannot close from {:?}", work.id, work.status);
        }
        let mut evidence = load_evidence(&transaction, &work.id)?;
        if evidence.completion_witness.is_some() {
            bail!("work `{}` already has a completion witness", work.id);
        }
        let closed_tree = witness.repository.tree_oid.0.clone();
        evidence.completion_witness = Some(witness);
        {
            let mut table = transaction.open_table(EVIDENCE)?;
            table.insert(work.id.0.as_str(), encode(&evidence)?.as_slice())?;
        }
        work.status = WorkStatus::Closed;
        {
            let mut works = transaction.open_table(WORK)?;
            works.insert(work.id.0.as_str(), encode(&work)?.as_slice())?;
        }
        let mut spec = load_spec(&transaction, &work.spec_id)?;
        spec.status = SpecStatus::Implemented;
        {
            let mut specs = transaction.open_table(SPECS)?;
            specs.insert(spec.id.0.as_str(), encode(&spec)?.as_slice())?;
        }
        append_event(
            &transaction,
            EventKind::WorkClosed,
            Some(&work.spec_id),
            Some(&work.id),
            Some(closed_tree),
        )?;
        {
            let mut state = transaction.open_table(STATE)?;
            state.remove(ACTIVE_KEY)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn events_for_work(&self, id: &WorkId) -> Result<Vec<TekoEvent>> {
        let transaction = self.database.begin_read()?;
        let table = transaction.open_table(EVENTS)?;
        table
            .iter()?
            .filter_map(|entry| match entry {
                Ok((_, value)) => match decode::<TekoEvent>(value.value(), "event") {
                    Ok(event) if event.work_id.as_ref() == Some(id) => Some(Ok(event)),
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                },
                Err(error) => Some(Err(error.into())),
            })
            .collect()
    }
}

fn load_spec(transaction: &redb::WriteTransaction, id: &SpecId) -> Result<Spec> {
    let table = transaction.open_table(SPECS)?;
    let bytes = table
        .get(id.0.as_str())?
        .ok_or_else(|| anyhow::anyhow!("spec `{id}` does not exist"))?;
    decode(bytes.value(), "spec")
}

fn load_work(transaction: &redb::WriteTransaction, id: &WorkId) -> Result<Work> {
    let table = transaction.open_table(WORK)?;
    let bytes = table
        .get(id.0.as_str())?
        .ok_or_else(|| anyhow::anyhow!("work `{id}` does not exist"))?;
    decode(bytes.value(), "work")
}

fn load_evidence(transaction: &redb::WriteTransaction, id: &WorkId) -> Result<EvidenceSet> {
    let table = transaction.open_table(EVIDENCE)?;
    let bytes = table
        .get(id.0.as_str())?
        .ok_or_else(|| anyhow::anyhow!("missing evidence for work `{id}`"))?;
    decode(bytes.value(), "evidence")
}

fn append_event(
    transaction: &redb::WriteTransaction,
    kind: EventKind,
    spec_id: Option<&SpecId>,
    work_id: Option<&WorkId>,
    detail: Option<String>,
) -> Result<TekoEvent> {
    let sequence = {
        let mut meta = transaction.open_table(META)?;
        let sequence = meta
            .get(EVENT_SEQUENCE_KEY)?
            .map(|value| value.value())
            .unwrap_or(0)
            + 1;
        meta.insert(EVENT_SEQUENCE_KEY, sequence)?;
        sequence
    };
    let event = TekoEvent {
        sequence,
        at: now_secs(),
        kind,
        spec_id: spec_id.cloned(),
        work_id: work_id.cloned(),
        detail,
    };
    let mut events = transaction.open_table(EVENTS)?;
    events.insert(sequence, encode(&event)?.as_slice())?;
    Ok(event)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(&Stored {
        schema: SCHEMA_VERSION,
        value,
    })
    .context("failed to serialize Teko store value")
}

fn decode<T: DeserializeOwned>(bytes: &[u8], label: &str) -> Result<T> {
    Ok(decode_stored::<T>(bytes, label)?.value)
}

fn decode_stored<T: DeserializeOwned>(bytes: &[u8], label: &str) -> Result<Stored<T>> {
    let stored: Stored<T> = serde_json::from_slice(bytes)
        .with_context(|| format!("corrupt Teko {label}: invalid JSON"))?;
    if stored.schema != SCHEMA_VERSION {
        bail!(
            "unsupported Teko {label} schema {}; this binary supports {}",
            stored.schema,
            SCHEMA_VERSION
        );
    }
    Ok(stored)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consistency::{ConflictState, HeadState};
    use crate::teko::{
        CompletionLevel, Goal, GoalId, Obligation, ObligationId, PathRule, RepositoryPoint, Scope,
        TreeOid,
    };

    fn point(tree: &str) -> RepositoryPoint {
        RepositoryPoint {
            token: crate::consistency::Consistency {
                algorithm: crate::consistency::CONSISTENCY_ALGORITHM.to_string(),
                head: HeadState::Normal {
                    branch: "main".to_string(),
                    oid: "head".to_string(),
                },
                index_fingerprint: "i".to_string(),
                worktree_fingerprint: "w".to_string(),
                untracked_fingerprint: "u".to_string(),
                git_operation: crate::consistency::GitOperationState::None,
                conflict_state: ConflictState {
                    has_conflicts: false,
                    files: Vec::new(),
                },
                token: format!("token-{tree}"),
            },
            tree_oid: TreeOid(tree.to_string()),
            branch: Some("main".to_string()),
        }
    }

    fn spec(status: SpecStatus) -> Spec {
        Spec {
            id: SpecId("spec".to_string()),
            title: "Teko".to_string(),
            objective: "make truth".to_string(),
            goals: vec![Goal {
                id: GoalId("goal".to_string()),
                description: "goal".to_string(),
            }],
            non_goals: Vec::new(),
            invariants: Vec::new(),
            verification: Vec::new(),
            completion: CompletionLevel::Changed,
            status,
            source: None,
        }
    }

    fn work(id: &str) -> Work {
        Work {
            id: WorkId(id.to_string()),
            spec_id: SpecId("spec".to_string()),
            objective: "make truth".to_string(),
            scope: Scope {
                write: vec![PathRule::new(PathBuf::from(".")).unwrap()],
            },
            obligations: vec![Obligation {
                id: ObligationId("manual".to_string()),
                description: "manual".to_string(),
                kind: ObligationKind::Manual,
            }],
            completion: CompletionLevel::Changed,
            origin: point("a"),
            status: WorkStatus::Open,
        }
    }

    #[test]
    fn spec_active_work_and_event_order_survive_restart() {
        let temp = tempfile::tempdir().unwrap();
        {
            let store = TekoStore::open(temp.path()).unwrap();
            store.create_spec(&spec(SpecStatus::Ratified)).unwrap();
            store.activate_work(work("one"), false).unwrap();
        }
        let store = TekoStore::open(temp.path()).unwrap();
        assert_eq!(
            store
                .get_spec(&SpecId("spec".to_string()))
                .unwrap()
                .unwrap()
                .title,
            "Teko"
        );
        let active = store.active_work().unwrap().unwrap();
        assert_eq!(active.id, WorkId("one".to_string()));
        let events = store.events_for_work(&active.id).unwrap();
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            vec![EventKind::WorkCreated, EventKind::WorkActivated]
        );
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[test]
    fn conflicting_activation_rolls_back_every_partial_write() {
        let temp = tempfile::tempdir().unwrap();
        let store = TekoStore::open(temp.path()).unwrap();
        store.create_spec(&spec(SpecStatus::Ratified)).unwrap();
        store.activate_work(work("one"), false).unwrap();
        let before = store.events_for_work(&WorkId("one".to_string())).unwrap();
        assert!(store.activate_work(work("two"), false).is_err());
        assert!(
            store
                .get_work(&WorkId("two".to_string()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.events_for_work(&WorkId("one".to_string())).unwrap(),
            before
        );
        assert_eq!(
            store.active_work().unwrap().unwrap().id,
            WorkId("one".to_string())
        );
    }

    #[test]
    fn draft_cannot_activate_without_explicit_ratification() {
        let temp = tempfile::tempdir().unwrap();
        let store = TekoStore::open(temp.path()).unwrap();
        store.create_spec(&spec(SpecStatus::Draft)).unwrap();
        assert!(store.activate_work(work("one"), false).is_err());
        assert!(store.active_work().unwrap().is_none());
        store.activate_work(work("one"), true).unwrap();
        assert_eq!(
            store
                .get_spec(&SpecId("spec".to_string()))
                .unwrap()
                .unwrap()
                .status,
            SpecStatus::Ratified
        );
    }

    #[test]
    fn close_atomically_stores_witness_and_clears_active() {
        let temp = tempfile::tempdir().unwrap();
        let store = TekoStore::open(temp.path()).unwrap();
        store.create_spec(&spec(SpecStatus::Ratified)).unwrap();
        let active = store.activate_work(work("one"), false).unwrap();
        store
            .close_work(CompletionWitness {
                work_id: active.id.clone(),
                repository: point("b"),
                closed_at: 2,
            })
            .unwrap();
        assert!(store.active_work().unwrap().is_none());
        assert_eq!(
            store.get_work(&active.id).unwrap().unwrap().status,
            WorkStatus::Closed
        );
        assert!(
            store
                .evidence(&active.id)
                .unwrap()
                .completion_witness
                .is_some()
        );
    }

    #[test]
    fn corrupt_value_fails_closed_with_its_record_name() {
        let temp = tempfile::tempdir().unwrap();
        let store = TekoStore::open(temp.path()).unwrap();
        let transaction = store.database.begin_write().unwrap();
        {
            let mut specs = transaction.open_table(SPECS).unwrap();
            specs.insert("broken", b"not json".as_slice()).unwrap();
        }
        transaction.commit().unwrap();
        let error = store.get_spec(&SpecId("broken".to_string())).unwrap_err();
        assert!(error.to_string().contains("corrupt Teko spec"));
    }
}
