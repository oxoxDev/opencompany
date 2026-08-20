//! [`HarnessRunTurn`]: the harness's [`RunTurn`] implementation.
//!
//! Wraps a [`HarnessPool`] and the [`HarnessDeps`] every turn draws on, and
//! forwards each [`RunTurn`] method to the matching `pool.run*` call — the one
//! place [`HarnessDeps`] is re-attached to the brain-agnostic delegation seam
//! (issue #176). Both are borrowed, so the shared delegation queue and steer
//! registry inside `deps` stay the very handles the
//! [`DelegationRunner`](crate::runtime::delegation::DelegationRunner) drains.
//!
//! Compiled only under `feature = "openhuman"`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::Result;
use crate::company::steer::SteerControl;
use crate::harness::run_trace::RunTraceSink;
use crate::harness::{HarnessDeps, HarnessPool, TurnOutcome};
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::delegation::RunTurn;

/// The built-in harness's [`RunTurn`]: re-attaches [`HarnessDeps`] onto each
/// pool turn.
///
/// Holds its pool and deps by `Arc` rather than by reference so it can live in
/// a [`HarnessRouter`](crate::harness::router::HarnessRouter) alongside the
/// other lanes. A company running two `built_in` harnesses has one of these per
/// harness, each over its own pool and its own provider — which is the whole
/// point of naming more than one.
pub struct HarnessRunTurn {
    pool: Arc<HarnessPool>,
    deps: Arc<HarnessDeps>,
}

impl HarnessRunTurn {
    /// Wraps a pool + deps as one harness lane.
    pub fn new(pool: Arc<HarnessPool>, deps: Arc<HarnessDeps>) -> Self {
        Self { pool, deps }
    }
}

#[async_trait]
impl RunTurn for HarnessRunTurn {
    async fn run(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        chat_id: Option<&str>,
    ) -> Result<TurnOutcome> {
        self.pool
            .run(company, agent_id, message, &self.deps, chat_id)
            .await
    }

    async fn run_steered(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        control: &SteerControl,
        chat_id: Option<&str>,
        run_sink: Option<Arc<RunTraceSink>>,
    ) -> Result<TurnOutcome> {
        self.pool
            .run_steered(
                company, agent_id, message, &self.deps, control, chat_id, run_sink,
            )
            .await
    }

    async fn run_steered_background(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        control: &SteerControl,
        run_sink: Option<Arc<RunTraceSink>>,
    ) -> Result<TurnOutcome> {
        self.pool
            .run_steered_background(company, agent_id, message, &self.deps, control, run_sink)
            .await
    }

    async fn run_background(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
    ) -> Result<TurnOutcome> {
        self.pool
            .run_background(company, agent_id, message, &self.deps)
            .await
    }

    async fn ensure(&self, company: &CompanyRecord) -> Result<()> {
        self.pool.ensure(company, &self.deps).await
    }
}
