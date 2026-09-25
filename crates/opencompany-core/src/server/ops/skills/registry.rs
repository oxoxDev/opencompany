//! `GET …/skills/registry` — the shared skill library the console's registry tab
//! browses.
//!
//! Its own module because the library is **host-global**: every other route in
//! this folder writes or reads one company's deltas, and this one reports what
//! any company could install. Split out when `skills.rs` reached the
//! source-layout cap, along the seam that was already there.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::AppState;
use crate::company::SkillDoc;
use crate::server::error::ApiError;
use crate::server::ops::{ScopedCompany, scoped};

use super::DEFAULT_CATEGORY;

/// The publisher stamped on shared-library skills (mirrors the GraphQL type).
const REGISTRY_PUBLISHER: &str = "OpenCompany";

/// Builds the registry route fragment.
///
/// `registry` is a static segment, so it wins over the sibling `{slug}` pattern
/// regardless of registration order.
pub(super) fn router() -> Router<AppState> {
    scoped("/skills/registry", get(list_registry))
}

/// One skill in the shared library, as the console's registry tab browses it.
///
/// Deliberately **metadata only** — no `body`. Mirrors the GraphQL
/// `RegistrySkill` type so the two transports agree field for field.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistrySkill {
    id: String,
    name: String,
    description: String,
    category: String,
    publisher: String,
    /// The library revision this entry ships, from frontmatter. `None` for a
    /// skill authored before `version` existed.
    version: Option<String>,
}

impl RegistrySkill {
    fn from_doc(doc: &SkillDoc) -> Self {
        Self {
            id: doc.slug.clone(),
            name: doc.name.clone(),
            description: doc.description.clone(),
            category: doc
                .category
                .clone()
                .unwrap_or_else(|| DEFAULT_CATEGORY.to_string()),
            publisher: REGISTRY_PUBLISHER.to_string(),
            version: doc.version.clone(),
        }
    }
}

/// `GET …/skills/registry` — the shared skill library, metadata only.
///
/// **Metadata only, by construction**: [`RegistrySkill`] has no `body` field, so
/// the payload stays flat regardless of how large the library grows. Install is
/// server-authoritative, so the client never needs a body — it posts a slug and
/// the host resolves the content.
///
/// Scoped (and so authorized) like every other console route even though the
/// library itself is host-global; the registry is not public.
async fn list_registry(
    State(state): State<AppState>,
    _company: ScopedCompany,
) -> Result<Json<Vec<RegistrySkill>>, ApiError> {
    Ok(Json(
        state
            .shared_skill_registry()?
            .iter()
            .map(RegistrySkill::from_doc)
            .collect(),
    ))
}
