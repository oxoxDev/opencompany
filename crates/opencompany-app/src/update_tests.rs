use super::*;

/// The real key from the vendored OpenHuman config, which is a public key
/// and published as one — it is in `vendor/openhuman/app/src-tauri/tauri.conf.json`.
/// Used here only as a specimen of the *shape*; nothing verifies against it.
const A_REAL_PUBKEY: &str =
    "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDc0OTREMjkxREFCNUIzRTEK";

#[test]
fn a_real_looking_key_is_configured() {
    assert!(is_configured(A_REAL_PUBKEY));
}

/// The updater public key this repository ships. First installed when
/// desktop auto-update landed (`846913029`); rotated to minisign key id
/// `C10A9CFAF2A579C7` when the desktop moved to the `com.tinyhumans.opencompany`
/// identifier and the signing secrets moved into the `Production` GitHub
/// environment. Builds carrying the previous key (id `7123DDC68477042C`)
/// cannot verify updates signed with this one, so they must be reinstalled
/// from a DMG once.
///
/// Pinned here on purpose. A minisign **public** key is meant to be
/// distributed — it is what a shipped binary verifies a release against,
/// and it is inert without the private half, which stays an operator
/// secret — so carrying it is not the leak the pre-auto-update version of
/// this test was guarding against.
const SHIPPED_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEMxMEE5Q0ZBRjJBNTc5QzcKUldUSGVhWHkrcHdLd1Z3c3lRMnZpVklpZlNhVk0vN09nK01vdDJwYzIrbkJBbzhlQXFobmZ5cWUK";

/// The config carries the key this repository intends, and no other.
///
/// # Why this replaced `the_shipped_placeholder_is_not_configured`
///
/// That test asserted `tauri.conf.json` never carries a usable key, back
/// when the repository shipped a placeholder and the real key was injected
/// at release time. `846913029` installed the key deliberately, and the two
/// then contradicted each other: the `Desktop` lane went red on main and,
/// because pull-request CI builds the merge commit, on every open PR at
/// once — where it reads to each author as though their own branch broke
/// the desktop build.
///
/// The old test invited exactly this edit: *"If this fails because somebody
/// pasted a real public key in, that is fine — but it has to be a
/// deliberate edit to this test, not a silent one to the config."* This is
/// that edit, and it keeps the half of the guarantee that still applies.
/// The point was never "no key"; it was "no *silent* change to the key", so
/// the assertion is now equality against a pinned constant. A rotation
/// still has to come here and say so, which is what the original was
/// protecting.
#[test]
fn the_shipped_updater_key_is_the_intended_one() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    let pubkey = config["plugins"]["updater"]["pubkey"].as_str().unwrap();

    assert!(
        is_configured(pubkey),
        "tauri.conf.json carries no usable updater pubkey ({pubkey}); \
         a shipped desktop build cannot verify an update without one",
    );
    assert_eq!(
        pubkey, SHIPPED_PUBKEY,
        "the updater pubkey in tauri.conf.json changed; rotating it is fine, \
         but update SHIPPED_PUBKEY in the same commit so the change is on \
         the record rather than silent",
    );
}

#[test]
fn an_empty_or_nonsense_key_is_not_configured() {
    assert!(!is_configured(""));
    assert!(!is_configured("REPLACE-ME"));
    // Base64 of something else entirely — shaped like a key, isn't one.
    assert!(!is_configured("bm90IGEga2V5"));
}

#[test]
fn a_transient_failure_with_budget_left_retries() {
    assert_eq!(
        classify(1, MAX_DOWNLOAD_ATTEMPTS, true),
        RetryDecision::Retry
    );
    assert_eq!(
        classify(2, MAX_DOWNLOAD_ATTEMPTS, true),
        RetryDecision::Retry
    );
}

#[test]
fn the_last_attempt_gives_up_even_when_transient() {
    assert_eq!(
        classify(MAX_DOWNLOAD_ATTEMPTS, MAX_DOWNLOAD_ATTEMPTS, true),
        RetryDecision::GiveUp
    );
    assert_eq!(
        classify(MAX_DOWNLOAD_ATTEMPTS + 1, MAX_DOWNLOAD_ATTEMPTS, true),
        RetryDecision::GiveUp
    );
}

#[test]
fn a_fatal_failure_never_retries() {
    // A bad signature on the first attempt is still a bad signature on the
    // third, and this is the assertion that says so.
    assert_eq!(
        classify(1, MAX_DOWNLOAD_ATTEMPTS, false),
        RetryDecision::GiveUp
    );
    assert_eq!(classify(1, 1, true), RetryDecision::GiveUp);
}

/// An unsuccessful HTTP status is a download that did not arrive.
///
/// The plugin maps every non-2xx response to the download request onto
/// `Error::Network`, so this is what a 503 from GitHub's asset CDN on
/// release day looks like by the time it reaches us — the busiest minute
/// this feature has, and the one the retry exists for. It is a formatted
/// string and nothing else, so a 404 lands in the same variant and is
/// retried too; three attempts and six seconds is the whole price of not
/// being able to tell them apart.
#[test]
fn an_unsuccessful_http_status_is_worth_asking_again() {
    let unavailable = tauri_plugin_updater::Error::Network(
        "Download request failed with status: 503 Service Unavailable".into(),
    );
    assert!(is_transient(&unavailable));
    assert_eq!(
        classify(1, MAX_DOWNLOAD_ATTEMPTS, is_transient(&unavailable)),
        RetryDecision::Retry
    );
}

/// Everything that is not the transport stays fatal.
///
/// The pairing matters more than either assertion: `TargetNotFound` is the
/// answer a Windows client gets from a macOS-only manifest, and a signature
/// failure is the answer a tampered bundle gets. Retrying either is work
/// that cannot succeed, and on the second it is work an attacker chooses.
#[test]
fn a_manifest_or_signature_failure_is_still_fatal() {
    assert!(!is_transient(&tauri_plugin_updater::Error::TargetNotFound(
        "windows-x86_64".into()
    )));
    assert!(!is_transient(&tauri_plugin_updater::Error::SignatureUtf8(
        "not base64".into()
    )));
}

#[test]
fn the_whole_budget_costs_seconds_not_minutes() {
    assert_eq!(backoff_for(1), Duration::from_secs(2));
    assert!(backoff_for(2) > backoff_for(1));
    let total: Duration = (1..MAX_DOWNLOAD_ATTEMPTS).map(backoff_for).sum();
    assert!(
        total <= Duration::from_secs(10),
        "total backoff stays small"
    );
}
