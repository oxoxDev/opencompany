//! Grants, delegation, policy-mode and reserved-id manifest tests
//! (split out of `manifest_tests.rs`).

use super::*;

fn parse(text: &str) -> CompanyManifest {
    toml::from_str(text).expect("valid toml")
}

fn write_bundle(company_toml: &str, agent_files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(MANIFEST_FILE), company_toml).expect("write manifest");
    if !agent_files.is_empty() {
        let agents = dir.path().join(super::super::agent_file::AGENTS_DIR);
        std::fs::create_dir_all(&agents).expect("agents dir");
        for (name, body) in agent_files {
            std::fs::write(agents.join(name), body).expect("write agent");
        }
    }
    dir
}

/// The compatibility rule: a bare `company.toml` with `[[agent]]` entries
/// Each tier is accepted by name from a `company.toml` (issue #560).
///
/// This is the test for the trap that adding `auto` set. The validator keeps
/// its own list of modes (`POLICY_MODES`) and runs *before*
/// `PolicyMode::parse` ever sees the string, so a tier added to the enum and
/// the parser but not to that list is rejected at load with "must be one of
/// …" — unreachable from the only place anybody sets it, while every test in
/// `harness::policy` still passes because they all construct a `Policy`
/// directly and never cross this boundary.
///
/// The mode words are **literals** on purpose. Deriving them from
/// `POLICY_MODES` — the first version of this test — passes vacuously when a
/// mode is missing from that list, because the missing case simply stops
/// being generated. Revert-and-check caught it; the literal cannot be
/// removed by the edit it is meant to detect.
///
/// `harness::policy` holds the matching direction: that `POLICY_MODES` and
/// the enum agree, so a tier cannot be added here and nowhere else.
#[test]
fn every_tier_is_accepted_by_name_from_a_company_toml() {
    for mode in ["readonly", "supervised", "auto", "full"] {
        let manifest = parse(&format!(
            "[company]\nname = \"X\"\n[policy]\nmode = \"{mode}\"\n"
        ));
        let problems = manifest.validate();
        assert!(
            problems.is_empty(),
            "`[policy].mode = \"{mode}\"` is a tier the runtime knows but the manifest \
             validator rejects — unreachable from a company.toml: {problems:?}"
        );
    }
}

/// An `access = "record"` grant to a built-in ledger whose `writers`
/// excludes this agent must not silently disagree — it is a manifest
/// error, not a refusal the agent discovers at call time.
#[test]
fn a_record_grant_disagreeing_with_a_builtins_writers_is_rejected() {
    let agents = vec![toml::from_str::<crate::company::Agent>(
        "id = \"intern\"\nrole = \"Intern\"\nledgers = [{ name = \"risks\", access = \"record\" }]\n",
    )
    .unwrap()];
    let risks = crate::ledger::parse(
        &serde_json::json!({
            "slug": "risks",
            "title": "Risks",
            "fields": [
                { "name": "id", "role": "id" },
                { "name": "risk", "role": "title" },
                { "name": "status", "role": "status" }
            ],
            "statuses": [{ "name": "open" }, { "name": "closed", "closed": true }],
            "writers": ["cfo"]
        }),
        true,
    )
    .unwrap();

    let problems = ledger_grant_problems(&agents, &[risks]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("agent `intern`"), "{}", problems[0]);
    assert!(problems[0].contains("`risks`"), "{}", problems[0]);
    assert!(problems[0].contains("writers"), "{}", problems[0]);
}

/// A `read` grant never conflicts with `writers` — only `record` implies
/// write access, so only `record` is checked.
#[test]
fn a_read_grant_never_conflicts_with_writers() {
    let agents = vec![toml::from_str::<crate::company::Agent>(
        "id = \"intern\"\nrole = \"Intern\"\nledgers = [{ name = \"risks\", access = \"read\" }]\n",
    )
    .unwrap()];
    let risks = crate::ledger::parse(
        &serde_json::json!({
            "slug": "risks",
            "title": "Risks",
            "fields": [
                { "name": "id", "role": "id" },
                { "name": "risk", "role": "title" },
                { "name": "status", "role": "status" }
            ],
            "statuses": [{ "name": "open" }, { "name": "closed", "closed": true }],
            "writers": ["cfo"]
        }),
        true,
    )
    .unwrap();

    assert!(ledger_grant_problems(&agents, &[risks]).is_empty());
}

/// A `delegates_to` entry must name a real desk (issue #176).
///
/// The failure this catches is silent at runtime rather than loud: a member
/// whose allowlist resolves to nothing still carries `delegate_to_desk`, and
/// every call it makes is refused as off-allowlist. The manifest is where
/// that is visible.
#[test]
fn rejects_a_delegates_to_entry_that_is_not_a_desk() {
    let manifest = parse(
        "[company]\nname = \"X\"\n\
         [[agent]]\nid = \"lead\"\nrole = \"Lead\"\ndelegates_to = [\"writer\"]\n\
         [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n\
         [[group_chat]]\nid = \"content\"\nname = \"Content desk\"\nmembers = [\"writer\"]\n",
    );
    let problems = manifest.validate();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("agent `lead`"), "{}", problems[0]);
    assert!(problems[0].contains("`writer`"), "{}", problems[0]);
    // The most common mistake is naming the teammate instead of the desk,
    // so the message has to say which vocabulary the field takes.
    assert!(problems[0].contains("teammate ids"), "{}", problems[0]);
}

/// Desk **ids**, desk **names**, and the `"*"` wildcard all resolve; an
/// empty entry is called out separately from an unknown one.
#[test]
fn accepts_desk_ids_names_and_the_wildcard_in_delegates_to() {
    let ok = parse(
        "[company]\nname = \"X\"\n\
         [[agent]]\nid = \"lead\"\nrole = \"Lead\"\ndelegates_to = [\"content\", \"Legal desk\", \"*\"]\n\
         [[group_chat]]\nid = \"content\"\nname = \"Content desk\"\n\
         [[group_chat]]\nid = \"legal\"\nname = \"Legal desk\"\n",
    );
    assert!(ok.validate().is_empty(), "{:?}", ok.validate());

    let blank = parse(
        "[company]\nname = \"X\"\n\
         [[agent]]\nid = \"lead\"\nrole = \"Lead\"\ndelegates_to = [\"  \"]\n\
         [[group_chat]]\nid = \"content\"\nname = \"Content desk\"\n",
    );
    let problems = blank.validate();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("empty entry"), "{}", problems[0]);
}

/// The depth knob is bounded on both sides (issue #176): `0` would mean
/// "delegation off" wearing a depth's clothes, and past the ceiling the
/// per-level fan-out cap compounds into a runaway.
#[test]
fn rejects_a_delegation_depth_outside_its_bounds() {
    for depth in ["0", "5"] {
        let manifest = parse(&format!(
            "[company]\nname = \"X\"\n[tools]\nmax_delegation_depth = {depth}\n"
        ));
        let problems = manifest.validate();
        assert_eq!(problems.len(), 1, "depth {depth}: {problems:?}");
        assert!(
            problems[0].contains("`[tools].max_delegation_depth`"),
            "{}",
            problems[0]
        );
        assert!(problems[0].contains("between 1 and 4"), "{}", problems[0]);
    }
    for depth in ["1", "2", "3", "4"] {
        let manifest = parse(&format!(
            "[company]\nname = \"X\"\n[tools]\nmax_delegation_depth = {depth}\n"
        ));
        assert!(
            manifest.validate().is_empty(),
            "depth {depth} must be accepted: {:?}",
            manifest.validate()
        );
    }
    // Absent is always fine and means the default.
    let bare = parse("[company]\nname = \"X\"\n");
    assert_eq!(bare.tools.max_delegation_depth, None);
    assert!(bare.validate().is_empty());
}

/// An existing manifest that names no `delegates_to` parses to the empty
/// allowlist, which is what keeps #176 a no-op for every company that did
/// not ask for it.
#[test]
fn delegates_to_defaults_to_empty() {
    let manifest = parse("[company]\nname = \"X\"\n[[agent]]\nid = \"a\"\nrole = \"A\"\n");
    assert!(manifest.agents[0].delegates_to.is_empty());
}

#[test]
fn rejects_bad_policy_mode_in_prosumer_language() {
    let manifest = parse("[company]\nname = \"X\"\n[policy]\nmode = \"supervized\"\n");
    let problems = manifest.validate();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("`[policy].mode`"));
    assert!(problems[0].contains("readonly, supervised, auto, full"));
    assert!(problems[0].contains("supervized"));
}

#[test]
fn rejects_non_snake_case_and_duplicate_ids() {
    let manifest = parse(
        r#"
        [company]
        name = "X"
        [[agent]]
        id = "BadId"
        role = "A"
        [[agent]]
        id = "dup"
        role = "B"
        [[agent]]
        id = "dup"
        role = "C"
        "#,
    );
    let problems = manifest.validate();
    assert!(problems.iter().any(|p| p.contains("snake_case")));
    assert!(problems.iter().any(|p| p.contains("more than once")));
}

/// Issue #1757: `operator` is reserved for the built-in, read-only
/// Operator system channel. A manifest desk claiming it would be
/// indistinguishable from the system channel in the desk list, and every
/// message sent there would be refused by `chat_and_emit`'s read-only
/// guard (`src/server/operator.rs`), which does not know or care where a
/// `chat_id == OPERATOR_CHANNEL` came from.
#[test]
fn rejects_a_group_chat_claiming_the_reserved_operator_id() {
    let manifest = parse(
        "[company]\nname = \"X\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"CEO\"\n\
         [[group_chat]]\nid = \"operator\"\nname = \"Operator\"\nmembers = [\"ceo\"]\n",
    );
    let problems = manifest.validate();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("reserved") && p.contains("operator")),
        "{problems:?}"
    );
}

/// Issue #1781 review (Codex P2): the id check alone is not enough —
/// `server::operator::resolve_desk` matches a desk by id *or*
/// case-insensitive name, so a desk at a harmless id but named "Operator"
/// shadows the system channel exactly as thoroughly as claiming the
/// literal id would: `GET {scope}/chat/history?desk=operator` (the
/// console's pinned read-only row) resolves to this desk instead of the
/// system feed, and its own writable transcript displays through the
/// identity the console assumes is read-only.
#[test]
fn rejects_a_group_chat_named_operator_even_with_a_harmless_id() {
    let manifest = parse(
        "[company]\nname = \"X\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"CEO\"\n\
         [[group_chat]]\nid = \"ops\"\nname = \"Operator\"\nmembers = [\"ceo\"]\n",
    );
    let problems = manifest.validate();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("reserved") && p.contains("Operator")),
        "{problems:?}"
    );
}

/// Case-insensitive, matching `resolve_desk`'s own fold — "operator" and
/// "OPERATOR" alias the same collision as "Operator" does.
#[test]
fn the_operator_name_reservation_folds_case() {
    let manifest = parse(
        "[company]\nname = \"X\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"CEO\"\n\
         [[group_chat]]\nid = \"ops\"\nname = \"operator\"\nmembers = [\"ceo\"]\n",
    );
    let problems = manifest.validate();
    assert!(
        problems.iter().any(|p| p.contains("reserved")),
        "{problems:?}"
    );
}

/// Follow-up to the group-chat guard above: `RESERVED_AGENT_IDS` already
/// stops a console-minted teammate from taking `system`
/// (`mint_agent_id`), but a manifest agent's id is read straight from the
/// TOML and this loop never consulted the same list — so a manifest could
/// still declare `id = "system"` and collide with the runtime's own
/// author id (`SYSTEM_AUTHOR`, issue #966): `senderOf` reads `agent_id`
/// by value and would render every subsequent system notice as that
/// teammate.
#[test]
fn rejects_a_manifest_agent_claiming_a_reserved_id() {
    let manifest = parse(
        "[company]\nname = \"X\"\n\
         [[agent]]\nid = \"system\"\nrole = \"whatever\"\n",
    );
    let problems = manifest.validate();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("reserved") && p.contains("system")),
        "{problems:?}"
    );
}

/// The same guard covers every entry in `RESERVED_AGENT_IDS`, not just
/// `system` — `operator`, `agents`, and `desks` are equally live manifest
/// agent ids until this check runs.
///
/// Lowercased before use: the reserved-id arm compares
/// `eq_ignore_ascii_case` on purpose (`RESERVED_AGENT_IDS`'s own doc),
/// because one entry — `DEFAULT_DESK`, `"General"` — is a prosumer display
/// string, not a slug. Every manifest agent id must already be snake_case
/// (checked one arm above this one), so submitting `"General"` verbatim
/// never reaches the reserved-id arm at all — it is rejected first, and
/// correctly, as an invalid id format. Lowercasing exercises the guard
/// through the one shape a manifest id can actually take, for every
/// reserved value including that one.
#[test]
fn rejects_every_reserved_id_as_a_manifest_agent_id() {
    for reserved in crate::ports::types::RESERVED_AGENT_IDS {
        let candidate = reserved.to_ascii_lowercase();
        let manifest = parse(&format!(
            "[company]\nname = \"X\"\n[[agent]]\nid = \"{candidate}\"\nrole = \"whatever\"\n"
        ));
        let problems = manifest.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("reserved") && p.to_ascii_lowercase().contains(&candidate)),
            "id {candidate:?} (reserved: {reserved:?}) should have been rejected: {problems:?}"
        );
    }
}

/// Issue #1781 review (Codex P1): `register_company`'s `serve` boot loop
/// reloads every company directory's `company.toml` on each restart, so
/// a company whose roster already grandfathers a teammate at a
/// [`RESERVED_AGENT_IDS`](crate::ports::types::RESERVED_AGENT_IDS) id —
/// `operator`, the case the rest of this codebase's grandfather-support
/// machinery (`channel.rs`, `operator.rs`, `delivery.rs`) exists to run
/// correctly — must still be able to boot. `from_path`, the strict
/// authoring-time loader, is proven first to still refuse it (unchanged
/// behavior, pinning the pre-fix failure this regresses against);
/// `from_path_for_reload` must accept the identical manifest.
#[test]
fn from_path_for_reload_grandfathers_a_manifest_agent_at_a_reserved_id() {
    let dir = write_bundle(
        "[company]\nname = \"Acme\"\n\n[[agent]]\nid = \"operator\"\nrole = \"Chief of Staff\"\n",
        &[],
    );

    let strict = CompanyManifest::from_path(dir.path());
    assert!(
        strict.is_err(),
        "sanity check: the strict authoring loader must still refuse this manifest, \
         or this test is not exercising the rule it claims to"
    );

    let reloaded = CompanyManifest::from_path_for_reload(dir.path())
        .expect("a company that already grandfathers an `operator` teammate must reboot");
    assert!(
        reloaded.agents.iter().any(|a| a.id == "operator"),
        "the grandfathered agent itself must still be loaded, not merely tolerated: {:?}",
        reloaded.agents
    );
}

/// The reload loader still enforces every other manifest rule — it
/// grandfathers exactly the reserved-agent-id collision, not validation
/// as a whole, so a company directory hand-edited into a genuinely
/// invalid shape (here, a duplicate agent id) must still refuse to boot.
#[test]
fn from_path_for_reload_still_refuses_an_unrelated_validation_problem() {
    let dir = write_bundle(
        "[company]\nname = \"Acme\"\n\n\
         [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n\n\
         [[agent]]\nid = \"writer\"\nrole = \"Also Writer\"\n",
        &[],
    );

    let err = CompanyManifest::from_path_for_reload(dir.path())
        .expect_err("a duplicate agent id must still be refused on reload");
    assert!(
        format!("{err}").contains("more than once"),
        "unexpected error: {err}"
    );
}

/// Issue #1781 review (Codex P1): the `operator` group-chat id/name
/// reservation (`rejects_a_group_chat_claiming_the_reserved_operator_id`
/// above) is the desk-side twin of the agent-id reservation
/// `from_path_for_reload_grandfathers_a_manifest_agent_at_a_reserved_id`
/// covers — both postdate real companies, since `operator` only became a
/// reserved system channel with issue #1757. The agent-id arm was gated
/// on `enforce_reserved_agent_ids`; this arm was not, so a company whose
/// desk list already declared `id = "operator"` before the reservation
/// shipped could reboot as an agent-only grandfather case but never as a
/// desk one — `register_company`'s `serve` boot loop would refuse it on
/// every restart. `from_path` is proven first to still refuse it
/// (pinning the pre-fix failure this regresses against);
/// `from_path_for_reload` must accept the identical manifest and keep
/// the desk itself loaded.
#[test]
fn from_path_for_reload_grandfathers_a_group_chat_at_the_reserved_operator_id() {
    let dir = write_bundle(
        "[company]\nname = \"Acme\"\n\n\
         [[agent]]\nid = \"ceo\"\nrole = \"CEO\"\n\n\
         [[group_chat]]\nid = \"operator\"\nname = \"Legacy Ops\"\nmembers = [\"ceo\"]\n",
        &[],
    );

    let strict = CompanyManifest::from_path(dir.path());
    assert!(
        strict.is_err(),
        "sanity check: the strict authoring loader must still refuse this manifest, \
         or this test is not exercising the rule it claims to"
    );

    let reloaded = CompanyManifest::from_path_for_reload(dir.path())
        .expect("a company that already has a desk at the `operator` id must reboot");
    assert!(
        reloaded.group_chats.iter().any(|c| c.id == "operator"),
        "the grandfathered desk itself must still be loaded, not merely tolerated: {:?}",
        reloaded.group_chats
    );
}

/// The name-collision twin of the test above: a desk at a harmless id but
/// named "Operator" shadows the system channel exactly as thoroughly
/// (`server::operator::resolve_desk` matches by id *or* case-insensitive
/// name — see `rejects_a_group_chat_named_operator_even_with_a_harmless_id`),
/// and was equally unconditional before this fix.
#[test]
fn from_path_for_reload_grandfathers_a_group_chat_named_operator() {
    let dir = write_bundle(
        "[company]\nname = \"Acme\"\n\n\
         [[agent]]\nid = \"ceo\"\nrole = \"CEO\"\n\n\
         [[group_chat]]\nid = \"legacy_ops\"\nname = \"Operator\"\nmembers = [\"ceo\"]\n",
        &[],
    );

    let strict = CompanyManifest::from_path(dir.path());
    assert!(
        strict.is_err(),
        "sanity check: the strict authoring loader must still refuse this manifest, \
         or this test is not exercising the rule it claims to"
    );

    let reloaded = CompanyManifest::from_path_for_reload(dir.path())
        .expect("a company that already has a desk named \"Operator\" must reboot");
    assert!(
        reloaded.group_chats.iter().any(|c| c.id == "legacy_ops"),
        "the grandfathered desk itself must still be loaded, not merely tolerated: {:?}",
        reloaded.group_chats
    );
}
