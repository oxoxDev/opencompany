use super::*;
use crate::ports::types::SecretValue;
use crate::store::fs::FsSecretStore;

async fn store(entries: &[(&str, &str)]) -> (Arc<dyn SecretStore>, CompanyId) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn SecretStore> = Arc::new(FsSecretStore::new(dir.keep()));
    let company = CompanyId::new("acme");
    for (key, value) in entries {
        store
            .set(&company, key, SecretValue(value.to_string()))
            .await
            .expect("set");
    }
    (store, company)
}

#[tokio::test]
async fn both_halves_are_required() {
    // Neither half alone is usable, and half-configured must fail closed
    // rather than call the wrong site or send no auth.
    let (only_site, company) = store(&[(SITE_SECRET, "acme-test")]).await;
    assert!(
        TenantChargebee::resolve(&only_site, &company)
            .await
            .expect("a readable store is not an error")
            .is_none()
    );

    let (only_key, company) = store(&[(API_KEY_SECRET, "cb_key")]).await;
    assert!(
        TenantChargebee::resolve(&only_key, &company)
            .await
            .expect("a readable store is not an error")
            .is_none()
    );

    let (neither, company) = store(&[]).await;
    assert!(
        TenantChargebee::resolve(&neither, &company)
            .await
            .expect("a readable store is not an error")
            .is_none()
    );
}

#[test]
fn the_fingerprint_moves_on_either_half_and_is_stable_otherwise() {
    // This function is the whole input to the roster staleness check, so a
    // half of the pair dropped out of the hash would silently stop
    // rebuilding: agents would keep authenticating with a revoked key until
    // the process restarted, with nothing failing to say so.
    let of = |site: &str, key: &str| {
        TenantChargebee::fingerprint(&Some(TenantChargebee {
            config: ChargebeeConfig {
                site: site.to_string(),
                api_key: key.to_string(),
            },
        }))
    };

    let base = of("acme-test", "cb_key");
    assert_eq!(base, of("acme-test", "cb_key"), "stable for one config");
    assert_ne!(base, of("acme-live", "cb_key"), "the site must count");
    assert_ne!(base, of("acme-test", "cb_rotated"), "the KEY must count");
    assert_ne!(
        base,
        TenantChargebee::fingerprint(&None),
        "connected and unconnected must differ"
    );
}

#[tokio::test]
async fn a_blank_secret_counts_as_absent() {
    // The console writing an empty string is a cleared field, not a
    // credential — resolving it would produce requests with no auth.
    let (store, company) = store(&[(SITE_SECRET, "acme-test"), (API_KEY_SECRET, "   ")]).await;
    assert!(
        TenantChargebee::resolve(&store, &company)
            .await
            .expect("a readable store is not an error")
            .is_none()
    );
}

#[tokio::test]
async fn a_complete_pair_resolves_and_never_exposes_the_key() {
    let (store, company) =
        store(&[(SITE_SECRET, " acme-test "), (API_KEY_SECRET, " cb_key ")]).await;
    let resolved = TenantChargebee::resolve(&store, &company)
        .await
        .expect("the store reads")
        .expect("both halves present");
    assert_eq!(resolved.site(), "acme-test", "whitespace is trimmed");
    // `site()` is the only accessor; there is deliberately no key getter,
    // and Debug must not become one by accident.
    assert!(
        !format!("{resolved:?}").contains("cb_key"),
        "the API key must not reach a Debug rendering"
    );
}

#[test]
fn the_five_tools_split_reads_from_writes() {
    use openhuman_core as oh;
    use tinytools::PermissionLevel;

    let config = TenantChargebee {
        config: ChargebeeConfig {
            site: "acme-test".to_string(),
            api_key: "cb_key".to_string(),
        },
    };
    let tools = live::chargebee_tools(&config);
    let by_name: Vec<(&str, PermissionLevel)> = tools
        .iter()
        .map(|t| (t.name(), t.permission_level()))
        .collect();
    assert_eq!(by_name.len(), 5);

    for (name, level) in by_name {
        let expected = match name {
            // Writes a real customer sees. Parks for approval.
            "chargebee_send_invoice" | "chargebee_create_customer" => PermissionLevel::Execute,
            // "Has Alan paid?" must not need a click.
            _ => PermissionLevel::ReadOnly,
        };
        assert_eq!(level, expected, "{name} permission level");
    }
}
