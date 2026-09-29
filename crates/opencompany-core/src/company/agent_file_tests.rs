use super::*;

/// Writes a bundle with the given `agents/` files and returns its root.
fn bundle(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let agents = dir.path().join(AGENTS_DIR);
    std::fs::create_dir_all(&agents).expect("agents dir");
    for (name, body) in files {
        let path = agents.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(path, body).expect("write");
    }
    dir
}

fn problems_of(err: crate::error::OpenCompanyError) -> Vec<String> {
    match err {
        crate::error::OpenCompanyError::ManifestInvalid { problems, .. } => problems,
        other => panic!("expected ManifestInvalid, got {other}"),
    }
}

#[test]
fn an_absent_or_empty_agents_directory_is_not_a_bundle_roster() {
    let empty = tempfile::tempdir().expect("tempdir");
    assert!(
        !has_agent_files(empty.path()),
        "a bundle with no `agents/` at all keeps its `[[agent]]` roster"
    );

    // Present but empty: still not a roster. Treating it as one would blank
    // the roster of a company whose `company.toml` has a perfectly good one.
    let dir = bundle(&[]);
    assert!(!has_agent_files(dir.path()));
}

#[test]
fn loads_agents_sorted_by_stem_not_directory_order() {
    // Written in an order that is not sorted, so a readdir-order
    // implementation would visibly disagree. Roster order decides which
    // teammate is the orchestrator when nobody is tagged, so this is
    // load-bearing rather than cosmetic.
    let dir = bundle(&[
        ("zara.toml", "role = \"Zara\"\n"),
        ("alice.toml", "role = \"Alice\"\n"),
        ("mike.toml", "role = \"Mike\"\n"),
    ]);

    let agents = load_agents(dir.path()).expect("loads");
    let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["alice", "mike", "zara"]);
}

#[test]
fn the_filename_is_the_id_and_a_declared_id_must_agree() {
    let dir = bundle(&[("copywriter.toml", "role = \"Copywriter\"\n")]);
    let agents = load_agents(dir.path()).expect("loads");
    assert_eq!(agents[0].id, "copywriter");

    let dir = bundle(&[(
        "copywriter.toml",
        "id = \"copy_writer\"\nrole = \"Copywriter\"\n",
    )]);
    let problems = problems_of(load_agents(dir.path()).expect_err("mismatched id is refused"));
    assert_eq!(problems.len(), 1);
    // The message must name both halves: an operator who renamed one of them
    // needs to know which one the runtime believed.
    assert!(problems[0].contains("copy_writer"), "{problems:?}");
    assert!(problems[0].contains("copywriter"), "{problems:?}");
}

#[test]
fn a_non_snake_case_filename_is_refused() {
    let dir = bundle(&[("CopyWriter.toml", "role = \"Copywriter\"\n")]);
    let problems = problems_of(load_agents(dir.path()).expect_err("refused"));
    assert!(
        problems[0].contains("snake_case"),
        "the message must say what shape is wanted: {problems:?}"
    );
}

#[test]
fn a_missing_role_is_refused() {
    let dir = bundle(&[("copywriter.toml", "description = \"writes\"\n")]);
    let problems = problems_of(load_agents(dir.path()).expect_err("refused"));
    assert!(problems[0].contains("`role`"), "{problems:?}");
}

#[test]
fn every_problem_across_every_file_is_reported_at_once() {
    // Two broken files, two distinct problems in the second. An operator
    // fixing one problem per run is the failure this collects to avoid.
    let dir = bundle(&[
        ("Bad_Name.toml", "role = \"Fine\"\n"),
        (
            "other.toml",
            "role = \"\"\nprompt_files = [\"nope.md\", \"../escape.md\"]\n",
        ),
    ]);
    let problems = problems_of(load_agents(dir.path()).expect_err("refused"));
    assert_eq!(problems.len(), 4, "{problems:?}");
}

#[test]
fn prompt_files_are_read_from_the_bundle() {
    let dir = bundle(&[
        (
            "copywriter.toml",
            "role = \"Copywriter\"\nprompt_files = [\"prompts/tone.md\"]\n",
        ),
        ("prompts/tone.md", "Lead with the reader's problem."),
    ]);

    let agents = load_agents(dir.path()).expect("loads");
    assert_eq!(
        agents[0].prompt_files_resolved,
        vec![(
            "prompts/tone.md".to_string(),
            "Lead with the reader's problem.".to_string()
        )]
    );
    // The declared list survives alongside the resolved bodies: the console
    // renders what the agent asked for, not just what it got.
    assert_eq!(agents[0].prompt_files, vec!["prompts/tone.md".to_string()]);
}

#[test]
fn a_missing_prompt_file_is_an_error_rather_than_a_skipped_entry() {
    // The whole point: a typo here would otherwise yield a role whose prompt
    // was written around a briefing it silently never received.
    let dir = bundle(&[(
        "copywriter.toml",
        "role = \"Copywriter\"\nprompt_files = [\"prompts/tone.md\"]\n",
    )]);
    let problems = problems_of(load_agents(dir.path()).expect_err("refused"));
    assert!(problems[0].contains("prompts/tone.md"), "{problems:?}");
    assert!(problems[0].contains("does not exist"), "{problems:?}");
}

#[test]
fn a_prompt_file_path_may_not_escape_the_agents_directory() {
    for escape in ["../secrets.md", "nested/../../secrets.md", "/etc/passwd"] {
        let dir = bundle(&[(
            "copywriter.toml",
            &format!("role = \"Copywriter\"\nprompt_files = [\"{escape}\"]\n"),
        )]);
        let problems = problems_of(
            load_agents(dir.path()).unwrap_err_or_else_panic(&format!("{escape} is refused")),
        );
        assert!(problems[0].contains("outside"), "{escape} → {problems:?}");
    }
}

/// The Windows spellings of an escape, which `is_absolute` does not catch.
///
/// On Windows `Path::new("/etc/passwd").is_absolute()` is **false** —
/// absoluteness there needs a prefix — so the guard leans on the `RootDir` and
/// `Prefix` component arms for these.
/// `a_prompt_file_path_may_not_escape_the_agents_directory` covers the rootless
/// form as a side effect of its `/etc/passwd` case, but only when it runs on
/// Windows; these spellings say so directly.
///
/// `#[cfg(windows)]` because the arms are unreachable on Unix: `is_absolute`
/// already subsumes `RootDir` there and `Component::Prefix` does not exist, so
/// on a Linux runner the inputs below parse as ordinary relative filenames and
/// asserting refusal would assert the wrong thing. That asymmetry is why the
/// guard could be wrong for as long as it was — no lane in CI runs this file on
/// Windows.
#[cfg(windows)]
#[test]
fn a_windows_rooted_or_prefixed_prompt_file_path_is_refused() {
    // TOML literal strings (single-quoted): a double-quoted "C:\outside" would
    // read \o as an escape and fail to parse before the guard is reached.
    for escape in [
        // `RootDir`: rooted but with no prefix, which is the form `is_absolute`
        // answers `false` to on Windows.
        r"\etc\passwd",
        // `Prefix(Disk)`.
        r"C:\outside\secrets.md",
        // `Prefix(UNC)` — **two** leading backslashes. One would be another
        // `RootDir` case wearing a UNC costume, which is what this line said
        // before and why the UNC arm was not actually covered.
        r"\\server\share\secrets.md",
    ] {
        let dir = bundle(&[(
            "copywriter.toml",
            &format!("role = \"Copywriter\"\nprompt_files = ['{escape}']\n"),
        )]);
        let problems = problems_of(
            load_agents(dir.path()).unwrap_err_or_else_panic(&format!("{escape} is refused")),
        );
        assert!(
            problems[0].contains("outside"),
            "{escape} was not refused: {problems:?}"
        );
    }
}

/// A per-file teammate's `model` is carried, like its `harness`.
///
/// `AgentFile` had `harness` but not `model`, so serde dropped the line as
/// an unknown key and the built `Agent` hardcoded `None`. The failure was
/// silent in both directions: the override never applied, and because
/// `CompanyManifest::validate` only sees what parsing produced, the file
/// was not refused either. A bundle could state a model, be accepted, and
/// run on the harness default.
#[test]
fn a_per_file_teammate_carries_its_model_override() {
    let dir = bundle(&[(
        "critic.toml",
        "role = \"Critic\"\nharness = \"laptop\"\nmodel = \"claude-opus-4-5\"\n",
    )]);
    let agents = load_agents(dir.path()).expect("loads");
    let critic = agents.iter().find(|a| a.id == "critic").expect("parsed");
    assert_eq!(critic.harness.as_deref(), Some("laptop"));
    assert_eq!(
        critic.model.as_deref(),
        Some("claude-opus-4-5"),
        "a model in the file must reach the agent, or it is neither honoured nor refused"
    );
}

/// The provider half of the pair (keys rework slice 3a, issue #2306)
/// reaches the built `Agent` the same way `model` does — same reasoning
/// as the test above: an unwired field is silently neither honoured nor
/// refused.
#[test]
fn a_per_file_teammate_carries_its_provider() {
    let dir = bundle(&[(
        "researcher.toml",
        "role = \"Researcher\"\nprovider = \"anthropic\"\nmodel = \"test-model-large\"\n",
    )]);
    let agents = load_agents(dir.path()).expect("loads");
    let researcher = agents
        .iter()
        .find(|a| a.id == "researcher")
        .expect("parsed");
    assert_eq!(researcher.provider.as_deref(), Some("anthropic"));
    assert_eq!(researcher.model.as_deref(), Some("test-model-large"));
}

#[test]
fn a_subdirectory_toml_is_a_document_not_a_teammate() {
    // `prompt_files` may point at a `.toml` briefing; descending into
    // subdirectories would try to parse it as a roster entry.
    let dir = bundle(&[
        ("copywriter.toml", "role = \"Copywriter\"\n"),
        ("prompts/reference.toml", "not = \"an agent\"\n"),
    ]);
    let agents = load_agents(dir.path()).expect("loads");
    let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["copywriter"]);
}

#[test]
fn the_full_enriched_shape_round_trips() {
    let dir = bundle(&[
        (
            "critic.toml",
            r#"
role = "Critic"
description = "Challenge a deliverable."
tier = "reasoning"
tools = ["docs.*", "mcp:notion"]
delegates_to = ["research"]
budget_usd_daily = 5.0
prompt = "Be specific about what would change your mind."
prompt_files = ["prompts/rubric.md"]
context = ["GOAL.md", "claims.md"]
classes = ["judge", "evidence"]
"#,
        ),
        ("prompts/rubric.md", "Score against the brief."),
    ]);

    let agent = load_agents(dir.path()).expect("loads").remove(0);
    assert_eq!(agent.id, "critic");
    assert_eq!(agent.tier.as_deref(), Some("reasoning"));
    assert_eq!(
        agent.tools,
        Some(vec!["docs.*".to_string(), "mcp:notion".to_string()])
    );
    assert_eq!(agent.delegates_to, ["research"]);
    assert_eq!(agent.budget_usd_daily, Some(5.0));
    assert_eq!(
        agent.prompt.as_deref(),
        Some("Be specific about what would change your mind.")
    );
    assert_eq!(
        agent.context.as_deref(),
        Some(
            &[
                crate::company::ContextEntry::from("GOAL.md"),
                crate::company::ContextEntry::from("claims.md")
            ][..]
        )
    );
    assert_eq!(agent.classes, ["judge", "evidence"]);
}

/// `context = []` must survive as an explicit empty list, distinct from an
/// omitted key — the routing layer reads the two differently.
#[test]
fn an_explicit_empty_context_is_distinct_from_an_omitted_one() {
    let dir = bundle(&[
        ("omitted.toml", "role = \"A\"\n"),
        ("explicit.toml", "role = \"B\"\ncontext = []\n"),
    ]);
    let agents = load_agents(dir.path()).expect("loads");
    let explicit = agents.iter().find(|a| a.id == "explicit").expect("found");
    let omitted = agents.iter().find(|a| a.id == "omitted").expect("found");
    assert_eq!(explicit.context, Some(Vec::new()));
    assert_eq!(omitted.context, None);
}

/// Small helper so the escape loop above reads as one assertion per case.
trait UnwrapErrOrPanic<T> {
    fn unwrap_err_or_else_panic(self, message: &str) -> crate::error::OpenCompanyError;
}

impl<T> UnwrapErrOrPanic<T> for Result<T> {
    fn unwrap_err_or_else_panic(self, message: &str) -> crate::error::OpenCompanyError {
        match self {
            Ok(_) => panic!("{message}"),
            Err(err) => err,
        }
    }
}
