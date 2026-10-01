//! `Company.skills { agents }` answers what `GET …/skills` answers.
//!
//! The two transports have disagreed about one skill row before — issue #239
//! found them reporting different `version`s — and the parity is asserted rather
//! than assumed for the same reason here: the console reads the REST list, so a
//! GraphQL projection that inverted the allowlist its own way would be wrong
//! with nothing looking at it.
//!
//! Compared whole-list rather than on one row. The failure this rules out is a
//! transport dropping an agent or a state, and a single-row assertion passes
//! while a whole column is missing.

use std::collections::BTreeMap;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::server::router;

use super::graphql_test_group_1::query;
use super::graphql_test_support_1::{home, state_with_manifest};

/// A roster in all three scope states, so the comparison has something to
/// disagree about. A manifest with no `skills` lines would make every agent
/// `inherited` and both transports right by accident.
const ROSTER: &str = r#"
[company]
name = "Acme"
[policy]
mode = "full"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "writer"
role = "Writer"
skills = ["email-drafting"]

[[agent]]
id = "hermit"
role = "Hermit"
skills = []
"#;

const SKILLS_QUERY: &str =
    r#"{"query":"{ company(id:\"acme\"){ skills { id enabled agents { id state holds } } } }"}"#;

/// Every row's `agents`, keyed by slug, out of one transport's answer.
fn by_slug(rows: &[Value]) -> BTreeMap<String, Value> {
    rows.iter()
        .map(|row| {
            (
                row["id"].as_str().expect("a slug").to_string(),
                row["agents"].clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn both_transports_report_the_same_per_skill_scope() {
    let home_dir = home();
    let manifest: CompanyManifest = toml::from_str(ROSTER).unwrap();
    let state = state_with_manifest(home_dir.path(), manifest).await;

    let gql = query(router(state.clone()), SKILLS_QUERY).await;
    let gql_rows = gql["data"]["company"]["skills"]
        .as_array()
        .unwrap_or_else(|| panic!("no skills in {gql}"))
        .clone();

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/companies/acme/skills")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let rest: Value = serde_json::from_slice(&bytes).unwrap();
    let rest_rows = rest.as_array().expect("an array").clone();

    assert!(
        !gql_rows.is_empty(),
        "the global baseline installs skills in every company: {gql}"
    );
    let gql_by_slug = by_slug(&gql_rows);
    let rest_by_slug = by_slug(&rest_rows);
    assert_eq!(
        gql_by_slug.keys().collect::<Vec<_>>(),
        rest_by_slug.keys().collect::<Vec<_>>(),
        "the two transports list the same skills"
    );
    for (slug, agents) in &gql_by_slug {
        assert_eq!(
            agents, &rest_by_slug[slug],
            "`{slug}` is scoped differently on the two transports"
        );
        let ids: Vec<&str> = agents
            .as_array()
            .expect("an array")
            .iter()
            .map(|agent| agent["id"].as_str().expect("an id"))
            .collect();
        assert!(
            ids.contains(&"ceo") && ids.contains(&"writer") && ids.contains(&"hermit"),
            "every roster agent gets a row on `{slug}`: {agents}"
        );
    }
}

/// The three states survive the GraphQL projection, which maps them through a
/// second vocabulary of its own.
///
/// Without this the parity test above would pass on a GraphQL arm that reported
/// the same wrong string as REST is not able to — REST serializes the enum, this
/// side writes the strings out by hand, so the mapping is code that can be
/// wrong on its own.
#[tokio::test]
async fn the_graphql_arm_names_the_three_states() {
    let home_dir = home();
    let manifest: CompanyManifest = toml::from_str(ROSTER).unwrap();
    let state = state_with_manifest(home_dir.path(), manifest).await;

    let gql = query(router(state), SKILLS_QUERY).await;
    let row = gql["data"]["company"]["skills"]
        .as_array()
        .unwrap_or_else(|| panic!("no skills in {gql}"))
        .iter()
        .find(|row| row["enabled"] == true)
        .unwrap_or_else(|| panic!("no enabled skill in {gql}"))
        .clone();

    let agent = |id: &str| {
        row["agents"]
            .as_array()
            .expect("an array")
            .iter()
            .find(|agent| agent["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no `{id}` in {row}"))
    };
    assert_eq!(agent("ceo")["state"], "inherited", "{row}");
    assert_eq!(agent("ceo")["holds"], true, "{row}");
    assert_eq!(
        agent("hermit")["state"],
        "excluded",
        "a deliberate empty scope is not an inherited one: {row}"
    );
    assert_eq!(agent("hermit")["holds"], false, "{row}");
}
