//! Turning a company's declared `[[harness]]` set into the engines that serve
//! it.
//!
//! One place decides, for every declared harness, whether this host can run it
//! and what runs it — so the runtime builder does not grow a second opinion
//! about which agent lands where.
//!
//! ## One pool per `built_in` harness
//!
//! Each `built_in` harness gets its own [`HarnessPool`] and its own
//! [`HarnessDeps`], differing in exactly two fields: the provider (scoped to
//! that harness's config and credential slots) and
//! [`serves`](HarnessDeps::serves), which narrows the pool to the agents bound
//! to it.
//!
//! The narrowing is what makes one-pool-per-harness affordable. Without it every
//! pool would build every agent, so a ten-agent roster across three harnesses
//! would stand up thirty live agents — each holding a model client — to use ten.
//!
//! ## What is declared but not runnable
//!
//! An `acp` harness has no engine here yet: its transports live in the desktop
//! shell and the runner lane, and neither is wired into the server build. Rather
//! than silently routing those agents somewhere else, the harness is recorded as
//! unavailable with the reason, and a turn bound to it fails saying so. Falling
//! back would be the worst outcome available — the turn would succeed, on a
//! model and a credential nobody chose.

use std::collections::HashSet;
use std::sync::Arc;

use crate::company::Harness;
use crate::company::inference::{EnvDefault, HarnessScope};
use crate::harness::built_in::provider::TenantProvider;
use crate::harness::built_in::run_turn::HarnessRunTurn;
use crate::harness::built_in::{HarnessDeps, HarnessPool};
use crate::ports::SecretStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::delegation::RunTurn;

/// The engines a company's declared harnesses resolve to on this host.
pub struct Lanes {
    /// Agents the **default** harness serves, when the company declares more
    /// than one. `None` means the whole roster — the single-harness case.
    pub default_serves: Option<HashSet<String>>,
    /// Every lane beyond the default: its harness id and the engine serving it.
    pub lanes: Vec<(String, Arc<dyn RunTurn>)>,
    /// Declared harnesses this host cannot run, and why.
    pub unavailable: Vec<(String, String)>,
}

/// Which agents are bound to `harness_id`, given the company's default.
fn agents_on(record: &CompanyRecord, harness_id: &str, default_harness: &str) -> HashSet<String> {
    let mut ids: HashSet<String> = record
        .manifest
        .agents
        .iter()
        .filter(|a| a.harness.as_deref().unwrap_or(default_harness) == harness_id)
        .map(|a| a.id.clone())
        .collect();
    // A console-created (overlay) teammate has no manifest row and therefore no
    // harness binding — `overlay_agent_to_manifest` hardcodes `harness: None` —
    // so every overlay runs on the default harness. Fold them into the default
    // lane's serve set, or a multi-harness company would build them on no pool
    // at all: the default pool's `serves` would exclude them, no other lane
    // claims them, and the roster would silently drop a teammate the console
    // is still showing.
    if harness_id == default_harness {
        ids.extend(record.overlay_agents.iter().map(|a| a.id.clone()));
    }
    ids
}

/// Builds the lanes for `record`, given the deps the **default** harness runs
/// on.
///
/// Returns no lanes at all for a company that declares no `[[harness]]` (or
/// declares exactly one): there is nothing to route, and the caller keeps its
/// single pool untouched. That is the path every existing company takes.
pub fn build(
    record: &CompanyRecord,
    base: &HarnessDeps,
    secrets: Arc<dyn SecretStore>,
    env_default: Option<EnvDefault>,
) -> Lanes {
    let declared = record.manifest.effective_harnesses();
    let default_harness = record.manifest.default_harness_id();

    if declared.len() <= 1 {
        return Lanes {
            default_serves: None,
            lanes: Vec::new(),
            unavailable: Vec::new(),
        };
    }

    let mut lanes = Vec::new();
    let mut unavailable = Vec::new();

    for harness in declared.iter().filter(|h| h.id != default_harness) {
        match harness.kind.as_str() {
            "built_in" => lanes.push((
                harness.id.clone(),
                built_in_lane(
                    record,
                    base,
                    &secrets,
                    env_default.clone(),
                    harness,
                    &default_harness,
                ),
            )),
            // The ACP transports are supplied by the desktop shell (a stdio
            // subprocess) and the runner lane (a socket); a server build has
            // neither, so there is nothing to hand a turn to.
            "acp" => unavailable.push((
                harness.id.clone(),
                "it is an ACP harness and this build has no ACP transport wired — \
                 run it from the desktop app, or bind these agents to a `built_in` harness"
                    .to_string(),
            )),
            other => unavailable.push((
                harness.id.clone(),
                format!("`{other}` is not a harness kind this build knows how to run"),
            )),
        }
    }

    Lanes {
        default_serves: Some(agents_on(record, &default_harness, &default_harness)),
        lanes,
        unavailable,
    }
}

/// One `built_in` lane: its own pool, over deps carrying its own provider and
/// narrowed to the agents bound to it.
fn built_in_lane(
    record: &CompanyRecord,
    base: &HarnessDeps,
    secrets: &Arc<dyn SecretStore>,
    env_default: Option<EnvDefault>,
    harness: &Harness,
    default_harness: &str,
) -> Arc<dyn RunTurn> {
    // Its own `[harness.inference]`, else the company-level `[inference]` — the
    // caller cannot pick, because only the harness knows whether it declared
    // one.
    let manifest_inference = harness
        .inference
        .clone()
        .unwrap_or_else(|| record.manifest.inference.clone());

    let provider = Arc::new(
        TenantProvider::new(
            record.id.clone(),
            secrets.clone(),
            manifest_inference,
            env_default,
        )
        .with_scope(HarnessScope::named(&harness.id)),
    );

    let mut deps = base.clone();
    deps.provider = provider;
    deps.serves = Some(agents_on(record, &harness.id, default_harness));

    Arc::new(HarnessRunTurn::new(
        Arc::new(HarnessPool::new()),
        Arc::new(deps),
    ))
}

/// The company id a lane set was built for. Exposed so a caller can assert it
/// wired the lanes it thinks it did.
pub fn company_of(record: &CompanyRecord) -> &CompanyId {
    &record.id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::types::OverlayAgent;

    /// A two-harness company with a console-created overlay teammate.
    fn record() -> CompanyRecord {
        let manifest: crate::company::CompanyManifest = toml::from_str(
            r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "researcher"
role = "Researcher"
harness = "deep"

[[harness]]
id = "embedded"
kind = "built_in"
default = true

[[harness]]
id = "deep"
kind = "built_in"
"#,
        )
        .expect("valid manifest");
        CompanyRecord {
            id: CompanyId::new("acme"),
            manifest,
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: vec![OverlayAgent {
                id: "writer".into(),
                name: "Writer".into(),
                role: "Content Writer".into(),
                description: None,
                tools: Vec::new(),
            }],
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
        }
    }

    /// The default lane serves the whole default-bound roster **including**
    /// every overlay teammate, whose only harness is the default.
    #[test]
    fn the_default_lane_serves_every_overlay_agent() {
        let rec = record();
        let default = agents_on(&rec, "embedded", "embedded");
        assert!(default.contains("ceo"));
        assert!(!default.contains("researcher"), "bound to the deep lane");
        assert!(
            default.contains("writer"),
            "a console-created teammate runs on the default harness"
        );

        // And the named lane must not claim it — the overlay is nobody's but
        // the default's.
        let deep = agents_on(&rec, "deep", "embedded");
        assert!(deep.contains("researcher"));
        assert!(!deep.contains("writer"));
        assert!(!deep.contains("ceo"));
    }
}
