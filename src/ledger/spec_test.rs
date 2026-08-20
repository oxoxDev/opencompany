//! What a declaration may and may not say.

use serde_json::json;

use super::*;

fn minimal() -> serde_json::Value {
    json!({
        "slug": "risks",
        "title": "Risks",
        "derived": "derived/RISKS.md",
        "fields": [
            { "name": "id", "role": "id" },
            { "name": "risk", "role": "title" },
            { "name": "status", "role": "status" }
        ],
        "statuses": [
            { "name": "open" },
            { "name": "closed", "closed": true, "needs_reason": true }
        ]
    })
}

#[test]
fn a_minimal_declaration_parses() {
    let spec = parse(&minimal(), false).expect("valid");
    assert_eq!(spec.slug, "risks");
    assert_eq!(spec.source, LedgerSource::Events);
    assert!(!spec.builtin);
    assert!(spec.is_closed("closed"));
    assert!(!spec.is_closed("open"));
    // A declaration naming no write path still tells its refused caller what to
    // do, or the write guard has nothing to say.
    assert!(spec.written_by.contains("record_entry"));
}

/// **`StatusSpec` itself stays snake_case both ways** (issue #1266's
/// near-miss): it is also what a declared ledger round-trips through in
/// every store backend, so `needs_reason` in must equal `needs_reason` out
/// or a save-then-reload silently drops the flag back to `false`. The
/// console-facing camelCase key lives on a wire-only DTO instead
/// (`server::ops::ledgers::LedgerStatusDto`), covered by
/// `ledgers_test.rs`'s `a_status_that_needs_a_reason_carries_camel_case_on_the_wire`.
#[test]
fn status_spec_round_trips_needs_reason_through_its_own_serde_unchanged() {
    let spec = parse(&minimal(), false).expect("declaration still parses with needs_reason");
    let status = spec.status("closed").expect("the closed status");
    assert!(status.needs_reason);

    let wire = serde_json::to_value(status).unwrap();
    assert_eq!(wire["needs_reason"], true);
    let restored: StatusSpec = serde_json::from_value(wire).unwrap();
    assert!(
        restored.needs_reason,
        "a store round-trip through this type's own serde must not lose the flag"
    );
}

#[test]
fn a_ledger_needs_exactly_one_id_field() {
    let mut document = minimal();
    document["fields"] = json!([
        { "name": "risk", "role": "title" },
        { "name": "status", "role": "status" }
    ]);
    let error = parse(&document, false).expect_err("no id");
    assert!(format!("{error}").contains("role `id`"));

    document["fields"] = json!([
        { "name": "id", "role": "id" },
        { "name": "other", "role": "id" }
    ]);
    assert!(parse(&document, false).is_err(), "two ids is not one id");
}

#[test]
fn a_ledger_needs_a_status() {
    let mut document = minimal();
    document["statuses"] = json!([]);
    let error = parse(&document, false).expect_err("no statuses");
    assert!(format!("{error}").contains("at least one status"));
}

/// A section filtering on a status nothing declares renders empty forever, and
/// nothing says so — which is why it is refused rather than rendered.
#[test]
fn a_section_may_not_filter_on_an_undeclared_status() {
    let mut document = minimal();
    document["sections"] = json!([
        { "heading": "Live", "statuses": ["opne"] }
    ]);
    let error = parse(&document, false).expect_err("typo'd status");
    let message = format!("{error}");
    assert!(message.contains("opne"), "{message}");
    assert!(message.contains("always be empty"), "{message}");
}

/// The one route back to the 86 KB file riemann's `docs/ledgers.md` records. A
/// declaration that could raise its own bound would reopen it.
#[test]
fn a_declaration_cannot_raise_its_own_bound() {
    let mut document = minimal();
    document["sections"] = json!([
        { "heading": "Everything", "cap": 4_000_000_000_u64 }
    ]);
    let spec = parse(&document, false).expect("clamped, not refused");
    assert_eq!(spec.sections[0].cap, super::super::budget::MAX_LISTED);
}

/// Clamped rather than refused, deliberately: a ledger stored when the bound
/// was looser must keep rendering after the bound tightens.
#[test]
fn normalize_reclamps_a_spec_loaded_back_off_the_store() {
    let mut spec = parse(&minimal(), false).expect("valid");
    spec.sections.push(Section {
        heading: "Everything".to_string(),
        blurb: String::new(),
        statuses: Vec::new(),
        cap: usize::MAX,
        order: Order::Recorded,
    });
    spec.normalize().expect("still valid");
    assert_eq!(spec.sections[0].cap, super::super::budget::MAX_LISTED);
}

#[test]
fn a_slug_is_a_slug() {
    assert_eq!(normalize_slug(" Risks ").expect("lowered"), "risks");
    assert!(normalize_slug("").is_err());
    assert!(normalize_slug("../etc/passwd").is_err());
    assert!(normalize_slug("has spaces").is_err());
    assert!(normalize_slug("-leading").is_err());
    assert!(normalize_slug("trailing-").is_err());
    assert!(normalize_slug(&"x".repeat(49)).is_err());
    assert!(normalize_slug("customer-promises").is_ok());
}

/// The derived path reaches the workspace tree, so the folder rule has to hold
/// on the way in as well as at the guard.
#[test]
fn a_derived_path_is_one_flat_file_under_the_derived_folder() {
    let mut document = minimal();
    for bad in [
        "RISKS.md",
        "notes/RISKS.md",
        "derived/nested/RISKS.md",
        "derived/../secrets.md",
        "derived/RISKS.txt",
        "derived/",
    ] {
        document["derived"] = json!(bad);
        assert!(
            parse(&document, false).is_err(),
            "`{bad}` should not be a derived path"
        );
    }
    document["derived"] = json!("derived/RISKS.md");
    assert!(parse(&document, false).is_ok());
}

/// A declaration that names no file still gets one, derived from its slug —
/// otherwise the commonest declaration a model writes fails on a field it had
/// no way to guess the shape of.
#[test]
fn an_unnamed_derived_path_is_derived_from_the_slug() {
    let mut document = minimal();
    document["derived"] = json!("");
    let spec = parse(&document, false).expect("valid");
    assert_eq!(spec.derived, "derived/RISKS.md");

    document["slug"] = json!("customer-promises");
    let spec = parse(&document, false).expect("valid");
    assert_eq!(spec.derived, "derived/CUSTOMER_PROMISES.md");
}

#[test]
fn duplicate_fields_and_statuses_are_refused() {
    let mut document = minimal();
    document["fields"] = json!([
        { "name": "id", "role": "id" },
        { "name": "Risk", "role": "title" },
        { "name": "risk", "role": "prose" }
    ]);
    assert!(parse(&document, false).is_err(), "two fields, one name");

    let mut document = minimal();
    document["statuses"] = json!([
        { "name": "open" },
        { "name": "Open" }
    ]);
    assert!(parse(&document, false).is_err(), "two statuses, one name");
}

#[test]
fn writers_are_an_allowlist_and_empty_means_anyone() {
    let mut document = minimal();
    assert!(
        parse(&document, false)
            .expect("valid")
            .writable_by("anyone")
    );
    document["writers"] = json!(["cfo", " ", "ops"]);
    let spec = parse(&document, false).expect("valid");
    assert_eq!(spec.writers, ["cfo", "ops"]);
    assert!(spec.writable_by("CFO"), "matching is case-insensitive");
    assert!(!spec.writable_by("intern"));
}

#[test]
fn an_order_name_that_is_not_one_says_which_ones_are() {
    assert_eq!(parse_order("recent").expect("known"), Order::Recent);
    assert_eq!(parse_order(" recorded ").expect("known"), Order::Recorded);
    let error = parse_order("newest").expect_err("unknown");
    let message = format!("{error}");
    assert!(message.contains("recorded"), "{message}");
    assert!(message.contains("recent"), "{message}");
}

#[test]
fn the_default_order_is_the_first_sections() {
    let mut document = minimal();
    document["sections"] = json!([
        { "heading": "Live", "statuses": ["open"], "order": "recent" },
        { "heading": "Settled", "statuses": ["closed"] }
    ]);
    let spec = parse(&document, false).expect("valid");
    assert_eq!(spec.default_order(), Order::Recent);
    // A ledger declaring no sections renders as everything did before ordering
    // existed.
    let bare = parse(&minimal(), false).expect("valid");
    assert_eq!(bare.default_order(), Order::Recorded);
}

#[test]
fn a_declaration_that_is_not_one_says_so_rather_than_defaulting() {
    let error = parse(&json!({ "slug": "risks" }), false).expect_err("no fields");
    assert!(format!("{error}").contains("not a ledger declaration"));
}

#[test]
fn caps_bound_the_shape_of_a_declaration() {
    let mut document = minimal();
    let mut fields = vec![json!({ "name": "id", "role": "id" })];
    for n in 0..MAX_FIELDS {
        fields.push(json!({ "name": format!("f{n}"), "role": "prose" }));
    }
    document["fields"] = json!(fields);
    assert!(parse(&document, false).is_err(), "past the field cap");
}
