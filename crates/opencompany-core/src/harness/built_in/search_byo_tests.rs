use super::*;
// The legacy flat keys are a test concern only now: the resolver reaches
// them through `company::search::store`'s entry-zero fallback rather than
// naming them.
use crate::company::search::{API_KEY_SECRET, ENDPOINT_SECRET, PROVIDER_SECRET};
use crate::error::Result;
use crate::ports::types::SecretValue;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

#[derive(Default)]
struct MemSecrets {
    map: Mutex<HashMap<String, String>>,
}

impl MemSecrets {
    fn with(pairs: &[(&str, &str)]) -> Arc<dyn SecretStore> {
        let store = MemSecrets::default();
        let mut map = store.map.lock().unwrap();
        for (key, value) in pairs {
            map.insert((*key).to_string(), (*value).to_string());
        }
        drop(map);
        Arc::new(store)
    }
}

#[async_trait]
impl SecretStore for MemSecrets {
    async fn get(&self, _company: &CompanyId, key: &str) -> Result<Option<SecretValue>> {
        Ok(self
            .map
            .lock()
            .unwrap()
            .get(key)
            .map(|value| SecretValue(value.clone())))
    }
    async fn set(&self, _company: &CompanyId, key: &str, value: SecretValue) -> Result<()> {
        self.map.lock().unwrap().insert(key.to_string(), value.0);
        Ok(())
    }
}

/// A store whose reads always fail — the transient-hiccup case.
struct BrokenSecrets;

#[async_trait]
impl SecretStore for BrokenSecrets {
    async fn get(&self, _company: &CompanyId, _key: &str) -> Result<Option<SecretValue>> {
        Err(crate::error::OpenCompanyError::Store("boom".into()))
    }
    async fn set(&self, _c: &CompanyId, _k: &str, _v: SecretValue) -> Result<()> {
        Err(crate::error::OpenCompanyError::Store("boom".into()))
    }
}

/// The list, the marker and the harness agree on which account is billed.
///
/// This is the seam the whole rework turns on: the console writes a
/// credential at `search/provider/<slug>/key`, and the harness must read it
/// from there. A harness still reading the flat `search/api_key` would see
/// an unconfigured company and fall back to managed search — the agents keep
/// searching, they just quietly stop using the account the operator pays
/// for, which is the silent half of the failure this change exists to end.
///
/// Every credential below is obviously fake.
#[tokio::test]
async fn the_marked_provider_is_the_one_the_harness_wires() {
    let secrets = MemSecrets::with(&[
        (
            crate::company::search::store::PROVIDER_INDEX_KEY,
            r#"[{"slug":"exa","enabled":true},{"slug":"brave","enabled":true}]"#,
        ),
        ("search/provider/exa/key", "exa-not-a-real-key"),
        ("search/provider/brave/key", "brave-not-a-real-key"),
        (crate::company::search::store::DEFAULT_PROVIDER_KEY, "brave"),
    ]);

    let resolved = TenantSearch::resolve(&secrets, &company())
        .await
        .expect("resolve")
        .expect("a marked provider resolves");
    assert_eq!(resolved.provider(), "brave");

    let tools = byo_search_tools(&resolved);
    assert!(
        tools
            .iter()
            .any(|tool| tool.name() == crate::harness::search::WEB_SEARCH_TOOL),
        "the canonical web_search name must be on the belt whichever provider answers"
    );
}

/// Moving the marker moves the account, with no key re-entered.
#[tokio::test]
async fn moving_the_marker_moves_which_credential_is_used() {
    let pairs: Vec<(&str, &str)> = vec![
        (
            crate::company::search::store::PROVIDER_INDEX_KEY,
            r#"[{"slug":"exa","enabled":true},{"slug":"brave","enabled":true}]"#,
        ),
        ("search/provider/exa/key", "exa-not-a-real-key"),
        ("search/provider/brave/key", "brave-not-a-real-key"),
        (crate::company::search::store::DEFAULT_PROVIDER_KEY, "exa"),
    ];
    let secrets = MemSecrets::with(&pairs);
    let first = TenantSearch::resolve(&secrets, &company())
        .await
        .expect("resolve")
        .expect("resolves");
    assert_eq!(first.provider(), "exa");

    secrets
        .set(
            &company(),
            crate::company::search::store::DEFAULT_PROVIDER_KEY,
            SecretValue("brave".to_string()),
        )
        .await
        .expect("set marker");

    let second = TenantSearch::resolve(&secrets, &company())
        .await
        .expect("resolve")
        .expect("resolves");
    assert_eq!(second.provider(), "brave");
    assert_ne!(
        TenantSearch::fingerprint(&Some(first)),
        TenantSearch::fingerprint(&Some(second)),
        "the roster must rebuild when the account changes, or the old credential keeps \
         authenticating until a restart"
    );
}

/// A company that configured search before the list existed keeps working.
#[tokio::test]
async fn the_legacy_flat_keys_still_wire_a_provider() {
    let secrets = MemSecrets::with(&[
        (crate::company::search::PROVIDER_SECRET, "exa"),
        (crate::company::search::API_KEY_SECRET, "exa-not-a-real-key"),
    ]);
    let resolved = TenantSearch::resolve(&secrets, &company())
        .await
        .expect("resolve")
        .expect("entry zero resolves with nothing migrated");
    assert_eq!(resolved.provider(), "exa");
}

fn company() -> CompanyId {
    CompanyId::new("acme")
}

fn names(config: &TenantSearch) -> Vec<String> {
    byo_search_tools(config)
        .iter()
        .map(|tool| tool.name().to_string())
        .collect()
}

/// The three states that must all mean "search through the managed
/// surface", because each one is an ordinary state of a settings page and
/// none of them should leave an agent unable to search.
#[tokio::test]
async fn nothing_configured_managed_and_a_missing_key_all_fall_back_to_managed() {
    let empty = MemSecrets::with(&[]);
    assert!(
        TenantSearch::resolve(&empty, &company())
            .await
            .unwrap()
            .is_none(),
        "an unconfigured company must fall back to managed"
    );

    let managed = MemSecrets::with(&[(PROVIDER_SECRET, "managed")]);
    assert!(
        TenantSearch::resolve(&managed, &company())
            .await
            .unwrap()
            .is_none(),
        "`managed` is the fallback, never a BYO connection"
    );

    let keyless = MemSecrets::with(&[(PROVIDER_SECRET, "brave")]);
    assert!(
        TenantSearch::resolve(&keyless, &company())
            .await
            .unwrap()
            .is_none(),
        "a BYO provider with no key must fall back rather than wire a keyless tool"
    );

    let unknown = MemSecrets::with(&[(PROVIDER_SECRET, "google"), (API_KEY_SECRET, "k")]);
    assert!(
        TenantSearch::resolve(&unknown, &company())
            .await
            .unwrap()
            .is_none(),
        "an unknown slug is not a connection"
    );
}

/// A store hiccup is an error, not "not configured": the two need opposite
/// responses from the caller.
#[tokio::test]
async fn a_store_read_failure_is_an_error_and_not_a_silent_fallback() {
    let broken: Arc<dyn SecretStore> = Arc::new(BrokenSecrets);
    assert!(TenantSearch::resolve(&broken, &company()).await.is_err());
}

#[tokio::test]
async fn searxng_resolves_on_an_endpoint_alone_and_not_on_a_key() {
    let key_only = MemSecrets::with(&[(PROVIDER_SECRET, "searxng"), (API_KEY_SECRET, "k")]);
    assert!(
        TenantSearch::resolve(&key_only, &company())
            .await
            .unwrap()
            .is_none(),
        "a SearXNG instance is addressed by URL; a key is not the missing half"
    );

    let with_endpoint = MemSecrets::with(&[
        (PROVIDER_SECRET, "searxng"),
        (ENDPOINT_SECRET, "https://searx.example"),
    ]);
    let resolved = TenantSearch::resolve(&with_endpoint, &company())
        .await
        .unwrap()
        .expect("an endpoint is the whole configuration for searxng");
    assert_eq!(resolved.provider(), "searxng");
}

/// Whichever provider is configured, the model is told about the same
/// `web_search` — the affordance the shipped research skills name.
#[test]
fn every_provider_presents_its_canonical_tool_as_web_search() {
    for (provider, endpoint) in [
        ("brave", None),
        ("exa", None),
        ("querit", None),
        ("searxng", Some("https://searx.example".to_string())),
    ] {
        let config = TenantSearch {
            provider: provider.to_string(),
            api_key: Some("test-key".to_string()),
            endpoint,
        };
        let names = names(&config);
        assert!(
            names.contains(&crate::harness::search::WEB_SEARCH_TOOL.to_string()),
            "{provider} wired {names:?} with no canonical web_search"
        );
    }
}

/// The alias wears the canonical name but the step timeline names the
/// engine — the humanized fallback would collapse every provider to a
/// provider-less "Web search".
#[test]
fn the_aliased_tool_step_label_names_the_provider() {
    for (provider, endpoint, label) in [
        ("brave", None, "Brave web search"),
        ("exa", None, "Exa web search"),
        ("querit", None, "Querit web search"),
        (
            "searxng",
            Some("https://searx.example".to_string()),
            "SearXNG web search",
        ),
    ] {
        let config = TenantSearch {
            provider: provider.to_string(),
            api_key: Some("test-key".to_string()),
            endpoint,
        };
        let tools = byo_search_tools(&config);
        let aliased = tools
            .iter()
            .find(|tool| tool.name() == crate::harness::search::WEB_SEARCH_TOOL)
            .expect("every provider wires a canonical web_search");
        assert_eq!(
            aliased.display_label(&serde_json::json!({})).as_deref(),
            Some(label),
            "{provider}"
        );
        // And it reaches the operator. Every provider is aliased to the one
        // canonical `web_search`, so the tool *name* the loop labels from
        // carries no provider at all — only the tool's own label, restored by
        // `StepLabels`, can tell these four rows apart.
        assert_eq!(step_label(&tools), label, "{provider}");
    }
}

/// The label an operator actually reads for a `web_search` row, folded the
/// way a real turn folds it: the loop supplies the humanized tool name,
/// [`StepLabels`](crate::harness::steps::StepLabels) restores what the tool
/// calls itself, and `fold_steps` renders the row.
fn step_label(tools: &[Box<dyn tinytools::Tool>]) -> String {
    use crate::harness::search::WEB_SEARCH_TOOL;
    use crate::harness::steps::{StepLabels, fold_steps};
    use openhuman_core as oh;

    let labels = StepLabels::from_tools(tools);
    let started = oh::agent::progress::AgentProgress::ToolCallStarted {
        call_id: "c1".into(),
        tool_name: WEB_SEARCH_TOOL.into(),
        arguments: serde_json::Value::Null,
        iteration: 1,
        display_label: Some(tinytools::humanize_tool_name(WEB_SEARCH_TOOL)),
        display_detail: None,
    };
    fold_steps(vec![labels.apply(started)])
        .first()
        .expect("a tool call folds to one step")
        .label
        .clone()
}

#[test]
fn the_provider_families_are_exactly_what_each_provider_supports() {
    let brave = TenantSearch {
        provider: "brave".to_string(),
        api_key: Some("k".to_string()),
        endpoint: None,
    };
    assert_eq!(
        names(&brave),
        vec![
            "web_search",
            "brave_news_search",
            "brave_image_search",
            "brave_video_search"
        ]
    );

    let exa = TenantSearch {
        provider: "exa".to_string(),
        api_key: Some("k".to_string()),
        endpoint: None,
    };
    assert_eq!(
        names(&exa),
        vec!["web_search", "exa_find_similar", "exa_get_contents"]
    );
}

/// Every name a provider family can put on a belt is accounted for by
/// [`BYO_SEARCH_TOOLS`], so the namespace mapping cannot fall behind a
/// provider somebody added.
#[test]
fn every_wired_name_is_declared_in_the_byo_tool_list() {
    for provider in ["brave", "exa", "querit", "searxng"] {
        let config = TenantSearch {
            provider: provider.to_string(),
            api_key: Some("k".to_string()),
            endpoint: Some("https://searx.example".to_string()),
        };
        for name in names(&config) {
            assert!(
                BYO_SEARCH_TOOLS.contains(&name.as_str()),
                "{provider} wires `{name}`, which BYO_SEARCH_TOOLS does not declare"
            );
        }
    }
}

/// An unknown slug that somehow reached the store wires nothing rather than
/// panicking or wiring a tool with no provider behind it.
#[test]
fn an_unknown_provider_wires_nothing() {
    let config = TenantSearch {
        provider: "google".to_string(),
        api_key: Some("k".to_string()),
        endpoint: None,
    };
    assert!(names(&config).is_empty());
}

/// A rotated key must move the fingerprint, or the roster keeps
/// authenticating with the credential the operator just replaced.
#[test]
fn the_fingerprint_moves_on_a_rotation_and_on_a_provider_switch() {
    let base = TenantSearch {
        provider: "exa".to_string(),
        api_key: Some("first".to_string()),
        endpoint: None,
    };
    let rotated = TenantSearch {
        api_key: Some("second".to_string()),
        ..base.clone()
    };
    let switched = TenantSearch {
        provider: "brave".to_string(),
        ..base.clone()
    };

    let fp = |config: &TenantSearch| TenantSearch::fingerprint(&Some(config.clone()));
    assert_ne!(fp(&base), fp(&rotated));
    assert_ne!(fp(&base), fp(&switched));
    assert_ne!(fp(&base), TenantSearch::fingerprint(&None));
    assert_eq!(fp(&base), fp(&base.clone()));
}

/// The key must never reach a log line.
#[test]
fn debug_redacts_the_key() {
    let rendered = format!(
        "{:?}",
        TenantSearch {
            provider: "exa".to_string(),
            api_key: Some("super-secret".to_string()),
            endpoint: None,
        }
    );
    assert!(!rendered.contains("super-secret"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}
