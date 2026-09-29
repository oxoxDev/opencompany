use super::*;

#[test]
fn a_session_is_named_for_its_company_and_its_teammate() {
    let key = openhuman_session_key(&CompanyId::new("acme"), "designer");
    assert_eq!(key, "acme:designer");
}

#[test]
fn two_teammates_of_one_company_are_two_sessions() {
    let company = CompanyId::new("acme");
    assert_ne!(
        openhuman_session_key(&company, "designer"),
        openhuman_session_key(&company, "engineer"),
        "one company's teammates must not share a session id — the whole \
         point is telling their turns apart on the bus"
    );
}

#[test]
fn one_teammate_id_in_two_companies_is_two_sessions() {
    // The process is multi-tenant and `agent_id` is unique only within a
    // company, so the company has to be in the key or two tenants' turns
    // arrive on the bus indistinguishable.
    assert_ne!(
        openhuman_session_key(&CompanyId::new("acme"), "designer"),
        openhuman_session_key(&CompanyId::new("globex"), "designer"),
    );
}

#[test]
fn the_key_is_stable_for_the_same_pair() {
    let company = CompanyId::new("acme");
    assert_eq!(
        openhuman_session_key(&company, "designer"),
        openhuman_session_key(&company, "designer"),
        "a roster rebuild must not rename a live session"
    );
}

#[test]
fn the_channel_is_not_the_builders_unlabelled_default() {
    assert_ne!(
        SESSION_CHANNEL, "internal",
        "`internal` is what openhuman calls a session nobody named"
    );
}

#[test]
fn a_runtime_agent_id_is_the_company_and_teammate_folded_to_the_runtime_alphabet() {
    assert_eq!(
        runtime_agent_id(&CompanyId::new("acme"), "designer"),
        "acme--designer"
    );
    assert_eq!(
        runtime_agent_id(&CompanyId::new("Acme Co"), "QA.Lead"),
        "acme-co--qa-lead"
    );
    // A leading character outside the runtime's alphabet gets a prefix
    // rather than a refusal at `Runtime::agent`.
    assert!(runtime_agent_id(&CompanyId::new("-x"), "y").starts_with('a'));
}

#[test]
fn a_long_runtime_agent_id_is_truncated_with_a_hash_that_keeps_it_unique() {
    let company = CompanyId::new("a".repeat(70));
    let one = runtime_agent_id(&company, "one");
    let two = runtime_agent_id(&company, "two");
    assert!(one.len() <= 64, "{one}");
    assert!(two.len() <= 64, "{two}");
    assert_ne!(
        one, two,
        "two ids sharing their first 56 bytes stay distinct"
    );
    assert!(
        one.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    );
}
