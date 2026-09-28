use super::*;
use crate::ports::types::SecretValue;
use crate::store::fs::FsSecretStore;

async fn secrets_with(entries: &[(&str, &str)]) -> (Arc<dyn SecretStore>, CompanyId) {
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
async fn both_halves_of_the_credential_are_required() {
    let (id_only, company) = secrets_with(&[(CLIENT_ID_SECRET, "AY_id")]).await;
    assert!(
        TenantPaypal::resolve(&id_only, &company)
            .await
            .expect("a readable store is not an error")
            .is_none()
    );

    let (secret_only, company) = secrets_with(&[(CLIENT_SECRET_SECRET, "EL_secret")]).await;
    assert!(
        TenantPaypal::resolve(&secret_only, &company)
            .await
            .expect("a readable store is not an error")
            .is_none()
    );

    let (neither, company) = secrets_with(&[]).await;
    assert!(
        TenantPaypal::resolve(&neither, &company)
            .await
            .expect("a readable store is not an error")
            .is_none()
    );
}

#[test]
fn the_fingerprint_moves_on_the_credential_and_on_the_environment() {
    // Symmetric with the Chargebee side, plus the environment: moving a
    // company from sandbox to live with the same keys must rebuild, or its
    // agents keep reading the wrong world's balance.
    let of = |id: &str, secret: &str, env: PaypalEnvironment| {
        TenantPaypal::fingerprint(&Some(TenantPaypal {
            config: crate::paypal::PaypalConfig {
                client_id: id.to_string(),
                client_secret: secret.to_string(),
                environment: env,
            },
        }))
    };

    let base = of("AY_id", "EL_secret", PaypalEnvironment::Sandbox);
    assert_eq!(
        base,
        of("AY_id", "EL_secret", PaypalEnvironment::Sandbox),
        "stable for one config"
    );
    assert_ne!(
        base,
        of("AY_other", "EL_secret", PaypalEnvironment::Sandbox)
    );
    assert_ne!(base, of("AY_id", "EL_rotated", PaypalEnvironment::Sandbox));
    assert_ne!(
        base,
        of("AY_id", "EL_secret", PaypalEnvironment::Live),
        "the environment must count on its own"
    );
    assert_ne!(base, TenantPaypal::fingerprint(&None));
}

#[tokio::test]
async fn an_unset_environment_resolves_to_sandbox() {
    // The safe default, and the one that matters most: an operator who never
    // touched the environment field must not be reading a live balance.
    let (store, company) = secrets_with(&[
        (CLIENT_ID_SECRET, "AY_id"),
        (CLIENT_SECRET_SECRET, "EL_secret"),
    ])
    .await;
    let resolved = TenantPaypal::resolve(&store, &company)
        .await
        .expect("the store reads")
        .expect("both halves present");
    assert_eq!(resolved.environment(), PaypalEnvironment::Sandbox);
}

#[tokio::test]
async fn live_is_reached_only_by_saying_live() {
    let (store, company) = secrets_with(&[
        (CLIENT_ID_SECRET, "AY_id"),
        (CLIENT_SECRET_SECRET, "EL_secret"),
        (ENVIRONMENT_SECRET, "live"),
    ])
    .await;
    let resolved = TenantPaypal::resolve(&store, &company)
        .await
        .expect("the store reads")
        .expect("resolves");
    assert_eq!(resolved.environment(), PaypalEnvironment::Live);

    // And a near-miss does not.
    let (typo, company) = secrets_with(&[
        (CLIENT_ID_SECRET, "AY_id"),
        (CLIENT_SECRET_SECRET, "EL_secret"),
        (ENVIRONMENT_SECRET, "Live-ish"),
    ])
    .await;
    let resolved = TenantPaypal::resolve(&typo, &company)
        .await
        .expect("the store reads")
        .expect("resolves");
    assert_eq!(resolved.environment(), PaypalEnvironment::Sandbox);
}

#[tokio::test]
async fn the_credential_never_reaches_a_debug_rendering() {
    let (store, company) = secrets_with(&[
        (CLIENT_ID_SECRET, "AY_id"),
        (CLIENT_SECRET_SECRET, "EL_secret"),
    ])
    .await;
    let resolved = TenantPaypal::resolve(&store, &company)
        .await
        .expect("the store reads")
        .expect("resolves");
    let rendered = format!("{resolved:?}");
    assert!(!rendered.contains("EL_secret"), "{rendered}");
    assert!(!rendered.contains("AY_id"), "{rendered}");
}

#[test]
fn both_tools_are_read_only() {
    use openhuman_core as oh;
    use tinytools::PermissionLevel;

    let config = TenantPaypal {
        config: crate::paypal::PaypalConfig {
            client_id: "AY_id".to_string(),
            client_secret: "EL_secret".to_string(),
            environment: PaypalEnvironment::Sandbox,
        },
    };
    let tools = live::paypal_tools(&config);
    assert_eq!(tools.len(), 2);
    for tool in &tools {
        // Nothing here moves money (see the module docs), so nothing parks.
        assert_eq!(
            tool.permission_level(),
            PermissionLevel::ReadOnly,
            "{}",
            tool.name()
        );
    }
}
