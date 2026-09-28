use super::*;

/// The shared native vocabulary is exactly `GATEABLE_NAMESPACES` minus the
/// third-party connection path (`composio`) and the raw-HTTP family the S2
/// deflection governs (`web`).
#[test]
fn native_capability_vocabulary_is_gateable_minus_composio_and_web() {
    let native: std::collections::HashSet<&str> =
        native_capability_namespaces().into_iter().collect();
    let expected: std::collections::HashSet<&str> = GATEABLE_NAMESPACES
        .iter()
        .copied()
        .filter(|ns| *ns != "composio" && *ns != "web")
        .collect();
    assert_eq!(native, expected);
    assert!(!native.contains("composio"));
    assert!(!native.contains("web"));
}

/// `grants_confer_native` mirrors the harness wiring gate: the real-money
/// `search`/`media` families need their explicit grant (a bare `*` confers
/// neither), and every other native namespace rides the ordinary rule a `*`
/// satisfies.
#[test]
fn grants_confer_native_mirrors_the_wiring_gate() {
    assert!(grants_confer_native(&["search".into()], "search"));
    assert!(!grants_confer_native(&["*".into()], "search"));
    assert!(!grants_confer_native(&["composio".into()], "search"));

    assert!(grants_confer_native(&["media".into()], "media"));
    assert!(!grants_confer_native(&["*".into()], "media"));

    assert!(grants_confer_native(&["*".into()], "shell"));
    assert!(grants_confer_native(&["shell".into()], "shell"));
    assert!(!grants_confer_native(&["search".into()], "shell"));
}

/// **T10 (issue #971).** A manifest that never mentions
/// `approval_ttl_hours` parses to `None` and serializes without the key —
/// byte-identical to a build that predates the field.
///
/// This is not a serde-formatting nicety, it is the guard on
/// `carry_policy_override`. That rule is `previous_seed == next_seed` over
/// the whole `[policy]` block, so any value this field acquires at parse
/// becomes part of the identity of a block nobody wrote — and the day the
/// default moves, every silent manifest's seed changes under it and the
/// operator's console `[policy]` override is discarded as though version
/// control had spoken. The absence has to survive parse, persist and
/// reload for that not to happen. See the field's own note.
#[test]
fn a_manifest_without_an_approval_ttl_round_trips_unchanged() {
    let silent: Policy = toml::from_str(
        r#"
        mode = "supervised"
        "#,
    )
    .expect("parse toml");
    assert_eq!(silent.approval_ttl_hours, None);

    // Byte-identical: the key is absent from the wire, not `null`.
    let json = serde_json::to_string(&silent).expect("serialize");
    assert!(
        !json.contains("approval_ttl_hours"),
        "a silent manifest must not gain the key on the wire: {json}"
    );

    // And the reload is `==` to the parse, which is the comparison
    // `carry_policy_override` actually runs.
    let back: Policy = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, silent);
    assert_eq!(
        serde_json::to_string(&back).expect("serialize"),
        json,
        "a persist/reload cycle must be a fixed point"
    );

    // A manifest that DOES configure it keeps the value across the same
    // cycle — the absence is meaningful, so presence must be too.
    let configured: Policy = toml::from_str(
        r#"
        mode = "supervised"
        approval_ttl_hours = 72
        "#,
    )
    .expect("parse toml");
    assert_eq!(configured.approval_ttl_hours, Some(72));
    let json = serde_json::to_string(&configured).expect("serialize");
    let back: Policy = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, configured);

    // The two are NOT equal, which is what makes the seed comparison able
    // to tell "operator configured a deadline" from "nobody said anything".
    assert_ne!(silent, configured);
}

/// Real-money `media` (issue #109) is granted ONLY by an explicit `media` /
/// `media.*` grant — never by the catch-all `*`. This wildcard exclusion is
/// the security property that keeps a broadly-permissioned company from
/// accidentally handing its agents a paid image/video generator.
#[test]
fn media_grant_requires_explicit_namespace_not_wildcard() {
    assert!(grants_media_explicit(&["media".into()]));
    assert!(grants_media_explicit(&["media.image".into()]));
    assert!(grants_media_explicit(&["web.*".into(), "media".into()]));
    // The catch-all `*` must NOT grant media.
    assert!(!grants_media_explicit(&["*".into()]));
    assert!(!grants_media_explicit(&["web.*".into()]));
    assert!(!grants_media_explicit(&[]));
    // A substring match ("mediation") must not count as the media namespace.
    assert!(!grants_media_explicit(&["mediation".into()]));
}

/// Per-tenant `composio` (issue #110) is granted ONLY by an explicit
/// `composio` / `composio.*` grant — never by the catch-all `*`. The tools
/// reach third-party accounts over a tenant OAuth token, so a broadly-
/// permissioned company must still opt into them by name.
#[test]
fn composio_grant_requires_explicit_namespace_not_wildcard() {
    assert!(grants_composio_explicit(&["composio".into()]));
    assert!(grants_composio_explicit(&["composio.gmail".into()]));
    assert!(grants_composio_explicit(&[
        "web.*".into(),
        "composio".into()
    ]));
    // The catch-all `*` must NOT grant composio.
    assert!(!grants_composio_explicit(&["*".into()]));
    assert!(!grants_composio_explicit(&["web.*".into()]));
    assert!(!grants_composio_explicit(&[]));
    // A substring match must not count as the composio namespace.
    assert!(!grants_composio_explicit(&["composiotools".into()]));
}

/// The operator-installed `mcp_registry` surface is granted ONLY by an
/// explicit `mcp_registry` / `mcp_registry.*` grant — never by the
/// catch-all `*`. `mcp_registry_tool_call` invokes an arbitrary tool on any
/// server the company has installed and connected, with no per-server
/// scoping, so a broadly-permissioned company must still opt into it by
/// name.
#[test]
fn mcp_registry_grant_requires_explicit_namespace_not_wildcard() {
    assert!(grants_mcp_registry_explicit(&["mcp_registry".into()]));
    assert!(grants_mcp_registry_explicit(
        &["mcp_registry.notion".into()]
    ));
    assert!(grants_mcp_registry_explicit(&[
        "web.*".into(),
        "mcp_registry".into()
    ]));
    // The catch-all `*` must NOT grant the registry surface.
    assert!(!grants_mcp_registry_explicit(&["*".into()]));
    assert!(!grants_mcp_registry_explicit(&["web.*".into()]));
    assert!(!grants_mcp_registry_explicit(&[]));
    // The per-server bridge namespace (`mcp:<name>`) is a different grant
    // and must not be mistaken for it.
    assert!(!grants_mcp_registry_explicit(&["mcp:notion".into()]));
    assert!(!grants_mcp_registry_explicit(&["mcp:*".into()]));
    // A substring match must not count as the registry namespace.
    assert!(!grants_mcp_registry_explicit(&["mcp_registryextra".into()]));
}

/// The `[tools.composio]` sub-section parses its toolkit allowlist and an
/// absent section defaults to open mode (empty list).
#[test]
fn tools_composio_section_parses_toolkits_and_defaults_empty() {
    let with_section: CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[tools.composio]\ntoolkits = [\"gmail\", \"slack\"]\n",
    )
    .unwrap();
    assert_eq!(
        with_section.tools.composio.toolkits,
        vec!["gmail".to_string(), "slack".to_string()]
    );
    let without: CompanyManifest = toml::from_str("[company]\nname = \"Acme\"\n").unwrap();
    assert!(without.tools.composio.toolkits.is_empty());
}

#[test]
fn a_stale_speech_section_is_refused_with_a_hint() {
    let text = "[company]\nname = \"Acme\"\n[speech]\ndisabled = true\n";
    let problem = CompanyManifest::legacy_speech_block(text).expect("refused");
    assert!(problem.contains("[speech]"), "{problem}");
    assert!(problem.contains("opencompany"), "{problem}");
    assert!(CompanyManifest::legacy_speech_block("[company]\nname = \"Acme\"\n").is_none());
}

// Guards the newly-added `Serialize` derive: a manifest with renamed
// `[[agent]]`/`[[schedule]]` arrays must survive a serialize→deserialize
// round-trip through JSON without dropping the renamed fields.
#[test]
fn manifest_serialize_deserialize_round_trips() {
    let toml_src = r#"
        [company]
        name = "Acme"
        output = "widgets"

        [[agent]]
        id = "ceo"
        role = "Chief"
        tools = ["email.send"]

        [[schedule]]
        cron = "0 9 * * *"
        prompt = "daily standup"

        [policy]
        mode = "supervised"
        auto_approve_under_usd = 5.0
    "#;
    let manifest: CompanyManifest = toml::from_str(toml_src).expect("parse toml");

    let json = serde_json::to_string(&manifest).expect("serialize");
    let back: CompanyManifest = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back.company.name, "Acme");
    assert_eq!(back.agents.len(), 1);
    assert_eq!(back.agents[0].id, "ceo");
    assert_eq!(back.schedules.len(), 1);
    assert_eq!(back.schedules[0].cron, "0 9 * * *");
    assert_eq!(back.policy.auto_approve_under_usd, Some(5.0));

    // The renamed arrays serialize under their manifest keys.
    let value = serde_json::to_value(&manifest).unwrap();
    assert!(value.get("agent").is_some());
    assert!(value.get("schedule").is_some());
}

/// `Agent.context` is `Option<Vec<String>>`, not a defaulted `Vec`,
/// specifically so an omitted `context` key and an explicit `context = []`
/// stay distinguishable (docs/spec/runtime/orchestration/alignment.md's
/// per-tier-default rule depends on this). Pin the manifest round-trip for
/// both spellings so a regression to a defaulted `Vec` — which would
/// collapse them back to the same value — fails a test instead of shipping
/// silently.
#[test]
fn agent_context_distinguishes_omitted_from_explicit_empty() {
    let omitted: Agent = toml::from_str(
        r#"
        id = "critic"
        role = "Critic"
        "#,
    )
    .expect("parse toml");
    assert_eq!(
        omitted.context, None,
        "an omitted `context` key MUST deserialize to None, not an empty vec"
    );

    let explicit_empty: Agent = toml::from_str(
        r#"
        id = "critic"
        role = "Critic"
        context = []
        "#,
    )
    .expect("parse toml");
    assert_eq!(
        explicit_empty.context,
        Some(vec![]),
        "an explicit `context = []` MUST deserialize to Some(vec![]), distinct from None"
    );

    let populated: Agent = toml::from_str(
        r#"
        id = "critic"
        role = "Critic"
        context = ["GOAL.md", "claims.md"]
        "#,
    )
    .expect("parse toml");
    assert_eq!(
        populated.context,
        Some(vec![
            ContextEntry::from("GOAL.md"),
            ContextEntry::from("claims.md")
        ])
    );

    // The distinction survives a JSON round-trip too, since the routing
    // layer this field feeds may cross that boundary (e.g. the console).
    let json = serde_json::to_string(&omitted).expect("serialize");
    let back: Agent = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.context, None);
}

/// A bare `context` string is `Read`; `{ path, access = "write" }` is the
/// only way to grant `Write`. `write_scope` collects exactly the write
/// entries, and `None` — either an omitted `context` key or a `context`
/// with no write entry — is unconfined, not "confined to nothing".
#[test]
fn write_scope_is_none_unless_a_context_entry_declares_write() {
    let omitted: Agent = toml::from_str("id = \"critic\"\nrole = \"Critic\"\n").unwrap();
    assert_eq!(
        omitted.write_scope(),
        None,
        "an omitted context key is unconfined"
    );

    let read_only: Agent =
        toml::from_str("id = \"critic\"\nrole = \"Critic\"\ncontext = [\"brand/Voice.md\"]\n")
            .unwrap();
    assert_eq!(
        read_only.write_scope(),
        None,
        "a read-only context list is unconfined, not confined to nothing"
    );

    let write_entry: Agent = toml::from_str(
        r#"
        id = "critic"
        role = "Critic"
        context = ["brand/Voice.md", { path = "agents/critic/notes.md", access = "write" }]
        "#,
    )
    .expect("parse toml");
    assert_eq!(
        write_entry.write_scope(),
        Some(vec!["agents/critic/notes.md".to_string()]),
        "only the declared write entry is in scope, not the read one"
    );
}

/// An omitted `ledgers` key is unrestricted `Record` access to every slug
/// — the tool surface every agent had before this field existed. A
/// declared list answers only for the slugs it names.
#[test]
fn ledger_access_defaults_to_unrestricted_record() {
    let unrestricted: Agent = toml::from_str("id = \"critic\"\nrole = \"Critic\"\n").unwrap();
    assert_eq!(
        unrestricted.ledger_access("tasks"),
        Some(LedgerAccess::Record)
    );
    assert_eq!(
        unrestricted.ledger_access("anything"),
        Some(LedgerAccess::Record)
    );

    let scoped: Agent = toml::from_str(
        r#"
        id = "critic"
        role = "Critic"
        ledgers = [
            { name = "tasks", access = "record" },
            { name = "decisions", access = "read" },
        ]
        "#,
    )
    .unwrap();
    assert_eq!(scoped.ledger_access("tasks"), Some(LedgerAccess::Record));
    assert_eq!(scoped.ledger_access("DECISIONS"), Some(LedgerAccess::Read));
    assert_eq!(
        scoped.ledger_access("goals"),
        None,
        "an undeclared slug is unreachable"
    );
}

/// A bare `{ name = "tasks" }` grant, with no `access` key, defaults to
/// `Read` — the safer of the two, so declaring a `ledgers` list without
/// stating an access level does not silently hand out write access.
#[test]
fn a_ledger_grant_with_no_access_key_defaults_to_read() {
    let agent: Agent =
        toml::from_str("id = \"critic\"\nrole = \"Critic\"\nledgers = [{ name = \"tasks\" }]\n")
            .unwrap();
    assert_eq!(agent.ledger_access("tasks"), Some(LedgerAccess::Read));
}

/// The `[plan]` section (issue #108) survives a TOML → struct → JSON → struct
/// round-trip, and an absent section deserializes to the not-set default.
#[test]
fn plan_section_round_trips_and_defaults() {
    let toml_src = r#"
        [company]
        name = "Acme"

        [plan]
        name = "starter"
        period = "monthly"
        total_tokens = 2000000

        [plan.token_budgets]
        web = 500000
    "#;
    let manifest: CompanyManifest = toml::from_str(toml_src).expect("parse toml");
    assert!(manifest.plan.is_set());
    assert_eq!(manifest.plan.name.as_deref(), Some("starter"));
    assert_eq!(manifest.plan.period, "monthly");
    assert_eq!(manifest.plan.token_budgets.get("web"), Some(&500_000));
    assert_eq!(manifest.plan.total_tokens, Some(2_000_000));

    let json = serde_json::to_string(&manifest).expect("serialize");
    let back: CompanyManifest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.plan.name.as_deref(), Some("starter"));
    assert_eq!(back.plan.period, "monthly");
    assert_eq!(back.plan.token_budgets.get("web"), Some(&500_000));
    assert_eq!(back.plan.total_tokens, Some(2_000_000));

    // An absent `[plan]` defaults to period-only, which is NOT set.
    let bare: CompanyManifest = toml::from_str("[company]\nname = \"Bare\"\n").unwrap();
    assert!(!bare.plan.is_set());
    assert_eq!(bare.plan.period, "daily");
    assert_eq!(bare.plan.total_tokens, None);
}

/// A `[plan]` carrying **only** a `total_tokens` ceiling (issue #188) — no
/// name, no per-namespace budgets — is still a set plan, so the total gate
/// engages on its own.
#[test]
fn plan_total_tokens_only_is_set() {
    let manifest: CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[plan]\ntotal_tokens = 1000\n").unwrap();
    assert!(manifest.plan.is_set(), "a total-only plan must be set");
    assert_eq!(manifest.plan.total_tokens, Some(1000));
    assert!(manifest.plan.name.is_none());
    assert!(manifest.plan.token_budgets.is_empty());
}

/// Helper: the grant list shape every predicate here takes.
fn grants(list: &[&str]) -> Vec<String> {
    list.iter().map(|g| g.to_string()).collect()
}

/// **The asymmetry, pinned.** A bare `*` confers publishing and does NOT
/// confer `repo`.
///
/// This test exists to stop a future tidy-up, not to describe a subtlety.
/// `grants_files_or_docs` sits in a file of `grants_*_explicit` siblings
/// that all reject `*`, and folding it into that family is the obvious
/// "consistency" edit. It would be a silent revocation: most shipped
/// manifests grant `*` and nothing else, so publishing would switch off for
/// them with no error anywhere — agents that can still write files and can
/// no longer deliver one.
#[test]
fn a_bare_wildcard_confers_publishing() {
    let wildcard = grants(&["*"]);
    assert!(
        grants_files_or_docs(&wildcard),
        "a bare `*` must confer publishing — it is what most shipped manifests grant"
    );
    // The ordinary namespace forms confer it too.
    for grant in ["files", "docs", "files.write", "docs.read"] {
        assert!(
            grants_files_or_docs(&grants(&[grant])),
            "`{grant}` must confer publishing"
        );
    }
}

/// The boundary rule, pinned against a naive `starts_with`.
///
/// A documentation-flavoured grant is not a grant on `docs`, and a
/// filesystem-flavoured one is not a grant on `files`. `docsy` and
/// `filesystem` are the cases a bare prefix test actually gets wrong: both
/// extend the namespace without stopping on a separator, so `starts_with`
/// accepts them and would hand `publish_artifact` (and, through the shared
/// `wants_files` gate, the whole file belt) to an agent the manifest never
/// granted it to. Issue #461 removed this class of disagreement by routing
/// every grant match through `extends_on_boundary`; this asserts the
/// publishing predicate is on that side of it.
#[test]
fn documentation_grant_does_not_confer_publishing() {
    for grant in [
        "documentation",
        "documentation.read",
        "docsy",
        "filesystem",
        "filesystem.wipe",
        "web",
        "shell",
    ] {
        assert!(
            !grants_files_or_docs(&grants(&[grant])),
            "`{grant}` is not a grant on the files/docs namespace"
        );
    }

    // …and the real `e2e_harness` allow list, which grants no file family,
    // confers nothing either.
    assert!(
        !grants_files_or_docs(&grants(&[
            "composio",
            "mcp:*",
            "workspace",
            "workspace.*",
            "web"
        ])),
        "the shipped e2e_harness grants confer no publishing"
    );
}
