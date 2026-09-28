//! The one transaction that puts an effect in front of the operator.
//!
//! A chat cycle, a workflow run and a hive seat all park the same way: count
//! the decision against the turn that is waiting, park on the gate, journal
//! it, mark the work unit's checkout as held, then tell every console.
//! [`ApprovalParker`] is that sequence, so the paths cannot drift apart.

use std::sync::Arc;

use crate::error::OpenCompanyError;
use crate::ports::types::{Actor, ActorKind, ApprovalId, CompanyEvent, CompanyId, Effect, Verdict};
use crate::ports::{ApprovalGate, EventLog, now_millis};
use crate::runtime::continuation::ContinuationQueue;
use crate::runtime::grants::GrantSet;
use crate::runtime::journal::{ApprovalConversation, RuntimeJournal, TaskLink};

/// Where a parked effect belongs: the card that owns it, the conversation to
/// continue in, and the turn blocked on it.
#[derive(Clone, Debug)]
pub struct ParkSite {
    /// The board card that owns the request, if any.
    pub task: TaskLink,
    /// The channel and thread root the continuation lands in.
    pub conversation: ApprovalConversation,
    /// The continuation key of the turn blocked on this decision, if one is.
    pub turn: Option<String>,
}

/// The handles one park needs, bundled so a caller holds all of them or none.
#[derive(Clone)]
pub struct ApprovalParker {
    approvals: Arc<dyn ApprovalGate>,
    journal: Arc<RuntimeJournal>,
    grants: GrantSet,
    continuations: ContinuationQueue,
    events: Arc<dyn EventLog>,
}

impl ApprovalParker {
    /// A parker over the runtime's own gate, journal, grants, continuation
    /// counter and event log.
    pub fn new(
        approvals: Arc<dyn ApprovalGate>,
        journal: Arc<RuntimeJournal>,
        grants: GrantSet,
        continuations: ContinuationQueue,
        events: Arc<dyn EventLog>,
    ) -> Self {
        Self {
            approvals,
            journal,
            grants,
            continuations,
            events,
        }
    }

    /// Parks `effect` for `company` at `site`, returning the approval's id.
    ///
    /// The turn's continuation slot is counted before anything can make the
    /// approval visible to a resolver, and released again if the park never
    /// becomes durable. A journal failure retracts the gate entry, so an
    /// approval is either both parked and journaled or neither. The console
    /// event is best-effort: the journal is the binding record.
    pub async fn park(
        &self,
        company: &CompanyId,
        effect: Effect,
        site: ParkSite,
    ) -> Result<ApprovalId, OpenCompanyError> {
        let ParkSite {
            task,
            conversation,
            turn,
        } = site;
        if let Some(turn) = turn.as_deref() {
            self.continuations.arm(turn);
        }
        let approval_id = match self.approvals.park(company, effect.clone()).await {
            Ok(id) => id,
            Err(err) => {
                self.release(turn.as_deref());
                return Err(err);
            }
        };
        let work = task.task_id().map(str::to_string).or_else(|| {
            conversation
                .thread
                .as_deref()
                .and_then(crate::runtime::cycle::sanitize_work_segment)
        });
        let thread = conversation.thread.clone();
        if let Err(err) = self
            .journal
            .record_parked(
                &approval_id,
                &effect,
                now_millis(),
                task,
                conversation,
                turn.clone(),
            )
            .await
        {
            self.retract(company, &approval_id).await;
            self.release(turn.as_deref());
            return Err(err);
        }
        if let Some(work) = work {
            self.grants.mark_pending(&approval_id, work);
        }
        if let Err(err) = self
            .events
            .append(
                company,
                CompanyEvent::ApprovalParked {
                    approval_id: approval_id.clone(),
                    effect_kind: effect.kind.clone(),
                    thread,
                },
            )
            .await
        {
            tracing::warn!(
                approval_id = %approval_id,
                error = %err,
                "approval parked and journaled, but its event-log entry failed",
            );
        }
        Ok(approval_id)
    }

    /// The turn key `approval_id` was parked under, if it was parked under one.
    #[must_use]
    pub fn turn_of(&self, approval_id: &ApprovalId) -> Option<String> {
        self.journal.approval_cycle(approval_id).flatten()
    }

    /// Releases a continuation slot armed for a card that will never exist.
    fn release(&self, turn: Option<&str>) {
        if let Some(turn) = turn {
            self.continuations.decide(turn, None);
        }
    }

    /// Takes a gate entry the journal failed to record back off the gate and
    /// out of the journal's in-memory queue. Both steps swallow their own
    /// errors; the journal error the caller returns is the one worth reading.
    async fn retract(&self, company: &CompanyId, approval_id: &ApprovalId) {
        if let Err(rollback) = self
            .approvals
            .resolve(
                approval_id,
                Verdict::Deny,
                Actor {
                    kind: ActorKind::System,
                    id: "approval-parker".to_string(),
                },
            )
            .await
        {
            tracing::error!(
                company = %company,
                error = %rollback,
                "a parked effect could not be journaled AND could not be retracted from the \
                 approval gate; it may linger in the queue until restart"
            );
        }
        let _ = self.journal.record_resolved(approval_id).await;
    }
}

#[cfg(test)]
#[path = "approval_park_tests.rs"]
mod tests;
