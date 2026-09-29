use super::*;

/// The exact string the hub's `GET /auth/key` receives, pinned whole.
///
/// Every value percent-encoded the same way, `scopes` included: the hub
/// reads it off the query string like any other parameter, and an encoder
/// applied to three of four values is the one that surprises somebody.
#[test]
fn a_key_grant_query_asks_for_its_scopes_beside_the_challenge() {
    assert_eq!(
        key_grant_query("https://acme.example.com/?company=acme", "chal-abc", "Acme"),
        "callback_url=https%3A%2F%2Facme.example.com%2F%3Fcompany%3Dacme\
         &code_challenge=chal-abc\
         &code_challenge_method=S256\
         &name=Acme\
         &scopes=connections"
    );
}

/// Without this name on the ask, a key minted to anything but a provisioned
/// tenant cannot drive `/agent-integrations/composio/*` at all.
#[test]
fn the_scopes_asked_for_name_connections() {
    assert!(
        KEY_GRANT_SCOPES.contains(&"connections"),
        "managed Composio 403s without it: {KEY_GRANT_SCOPES:?}"
    );
}

/// Both readers get the builder's output verbatim.
///
/// The property the split exists for: neither path may append a parameter
/// of its own, because a grant whose reach depends on which page the
/// browser went through is one nobody can reason about from the consent
/// screen.
#[test]
fn both_readers_carry_the_same_built_query() {
    let query = key_grant_query("http://127.0.0.1:5173/?key=link", "chal-abc", "Acme");

    let api = key_grant_url(
        "https://api.example.com",
        "http://127.0.0.1:5173/?key=link",
        "chal-abc",
        "Acme",
    );
    let site = crate::server::hub_account::connect_url("https://example.com", &query);

    assert_eq!(api, format!("https://api.example.com/auth/key?{query}"));
    assert_eq!(site, format!("https://example.com/connect?{query}"));
}
