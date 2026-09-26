use super::*;
use crate::ports::types::CompanyId;

fn names() -> DisplayNames {
    let mut record = CompanyRecord::from_manifest(
        CompanyId::new("acme"),
        toml::from_str(
            r#"
[company]
name = "Acme"

[[agent]]
id = "qa_engineer"
role = "QA Engineer"

[[agent]]
id = "backend-dev"
role = "Backend Engineer"

[[agent]]
id = "pm"
role = "Product Manager"

[[group_chat]]
id = "eng_desk"
name = "Engineering"
members = ["qa_engineer", "backend-dev"]
"#,
        )
        .expect("valid manifest"),
    );
    record
        .overlay_agent_edits
        .push(crate::ports::types::AgentOverride {
            agent_id: "qa_engineer".to_string(),
            name: Some("Quinn".to_string()),
            ..Default::default()
        });
    DisplayNames::from_record(&record)
}

fn shown(text: &str) -> String {
    readable_moves(text.to_string(), &names())
}

#[test]
fn projection_table() {
    let cases: [(&str, &str, &str); 12] = [
        (
            "backticked id",
            "Ask `qa_engineer` first.",
            "Ask Quinn first.",
        ),
        ("at id", "Thanks @qa_engineer!", "Thanks Quinn!"),
        (
            "bare id with underscore",
            "qa_engineer has the failing case.",
            "Quinn has the failing case.",
        ),
        (
            "bare id with hyphen, no name falls back to role",
            "backend-dev owns it",
            "Backend Engineer owns it",
        ),
        ("desk id", "Posted to eng_desk.", "Posted to Engineering."),
        ("backticked desk id", "on `eng_desk`", "on Engineering"),
        (
            "bare id without separator stays",
            "pm will decide",
            "pm will decide",
        ),
        ("backticked short id", "ask `pm`", "ask Product Manager"),
        (
            "unknown id untouched",
            "ask `data_scientist` or ops_lead",
            "ask `data_scientist` or ops_lead",
        ),
        (
            "paths, files and addresses untouched",
            "see /agents/qa_engineer/notes and qa_engineer.md, mail qa_engineer@acme.io",
            "see /agents/qa_engineer/notes and qa_engineer.md, mail qa_engineer@acme.io",
        ),
        (
            "conversation prefix stripped",
            "[conversation: eng_desk, thread 12]\nShipped.",
            "Shipped.",
        ),
        (
            "other inline code untouched",
            "run `cargo test` then ping qa_engineer",
            "run `cargo test` then ping Quinn",
        ),
    ];
    for (label, input, expected) in cases {
        assert_eq!(shown(input), expected, "{label}");
    }
}

#[test]
fn fenced_code_is_untouched() {
    let text = "Here:\n```\nassign qa_engineer\n```\nqa_engineer is on it.";
    assert_eq!(
        shown(text),
        "Here:\n```\nassign qa_engineer\n```\nQuinn is on it."
    );
}

#[test]
fn a_shorter_or_mismatched_fence_does_not_close_the_block() {
    let text = "````\nagent: qa_engineer\n```\nstill inside\n````\nqa_engineer is on it.";
    assert_eq!(
        shown(text),
        "````\nagent: qa_engineer\n```\nstill inside\n````\nQuinn is on it."
    );
    let text = "```\nagent: qa_engineer\n~~~\nstill inside\n```\nqa_engineer is on it.";
    assert_eq!(
        shown(text),
        "```\nagent: qa_engineer\n~~~\nstill inside\n```\nQuinn is on it."
    );
}

#[test]
fn projecting_twice_changes_nothing() {
    for text in [
        "[conversation: a]\n[conversation: b]\n@qa_engineer and `eng_desk`, backend-dev.",
        "plain words",
        "",
    ] {
        let once = shown(text);
        assert_eq!(shown(&once), once, "{text:?}");
    }
}

#[test]
fn a_name_that_is_another_id_is_not_chained() {
    let names = DisplayNames::from_pairs([
        ("a_one".to_string(), "b_two".to_string()),
        ("b_two".to_string(), "Bee".to_string()),
    ]);
    assert_eq!(names.name_of("a_one"), None);
    let once = readable_moves("a_one b_two".to_string(), &names);
    assert_eq!(once, "a_one Bee");
    assert_eq!(readable_moves(once.clone(), &names), once);
}

#[test]
fn protected_spans_stay_and_later_offsets_follow_the_rewrite() {
    let text = "`qa_engineer` pinged @backend-dev about it";
    let chip = text.find("@backend-dev").unwrap();
    let span = chip..chip + "@backend-dev".len();
    let projected = project(text, &names(), std::slice::from_ref(&span));
    assert_eq!(projected.text, "Quinn pinged @backend-dev about it");
    let moved = projected.offset(chip);
    assert_eq!(
        &projected.text[moved..moved + "@backend-dev".len()],
        "@backend-dev"
    );
    assert_eq!(projected.offset(0), 0);
}

#[test]
fn no_names_means_no_change_beyond_the_prefix() {
    let empty = DisplayNames::default();
    assert_eq!(
        readable_moves("[conversation: x]\n`qa_engineer`".to_string(), &empty),
        "`qa_engineer`"
    );
}
