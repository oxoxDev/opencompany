use super::*;

#[test]
fn leading_imperative_is_actionable() {
    for msg in [
        "Build the landing page",
        "create a new email campaign",
        "fix the checkout bug",
        "find three suppliers for widgets",
        "set up the weekly newsletter",
        "look into why signups dropped",
        "research competitors in the EU",
    ] {
        assert!(detect_task_intent(msg).is_some(), "should fire: {msg}");
    }
}

#[test]
fn relaying_a_greeting_is_answering_with_tools_not_trackable_work() {
    for message in [
        "can you tell johnny hi?",
        "tell backend_engineer hello",
        "please send alice a hey",
    ] {
        assert_eq!(triage_message(message), MessageTriage::Answer, "{message}");
    }
}

#[test]
fn framed_request_with_action_verb_is_actionable() {
    assert_eq!(
        detect_task_intent("Can you build the landing page?").as_deref(),
        Some("Build the landing page")
    );
    assert!(detect_task_intent("please set up the newsletter").is_some());
    assert!(detect_task_intent("I need you to draft the pitch").is_some());
    assert!(detect_task_intent("let's launch the beta").is_some());
}

#[test]
fn pure_questions_do_not_fire() {
    for msg in [
        "what's our revenue this month?",
        "how many users signed up?",
        "who is on the growth desk?",
        "can you tell me the latest numbers?", // framed, but no action verb
        "is the campaign live?",
        "why did signups drop?",
    ] {
        assert!(detect_task_intent(msg).is_none(), "should not fire: {msg}");
        assert_eq!(
            triage_message(msg),
            MessageTriage::Answer,
            "should be an answer: {msg}"
        );
    }
}

/// Issue #267: the six cards found sitting unworked in `backlog` on a live
/// company, each pinned to the class it now triages as.
///
/// Five of the six lead with an action verb, so they came through the
/// deterministic handler path and stay `Track` — they *are* instructions,
/// and the fix for the four workflow asks is that the orchestrator now
/// authors the graph in-turn rather than parking the card. The sixth has no
/// action verb and no request frame, so this module never saw it: it was
/// the model's own `spawn_task`, and `Answer` is what takes that tool away.
#[test]
fn the_six_observed_dead_cards_triage_as_recorded() {
    let cases: &[(&str, MessageTriage)] = &[
        (
            "Tell what is there in the tasks list",
            MessageTriage::Answer,
        ),
        (
            "Call the composio_authorize tool with toolkit = gmail and reply with the result",
            MessageTriage::Track(
                "Call the composio_authorize tool with toolkit = gmail and reply with the \
                 result"
                    .to_string(),
            ),
        ),
        (
            "Create a workflow to, write a 2-sentence status update",
            MessageTriage::Track(
                "Create a workflow to, write a 2-sentence status update".to_string(),
            ),
        ),
        (
            "Create a simple workflow, Write a 2-sentence status update",
            MessageTriage::Track(
                "Create a simple workflow, Write a 2-sentence status update".to_string(),
            ),
        ),
        (
            "Create a workflow named topic-to-visual",
            MessageTriage::Track("Create a workflow named topic-to-visual".to_string()),
        ),
        (
            "Create a workflow called Daily Standup",
            MessageTriage::Track("Create a workflow called Daily Standup".to_string()),
        ),
    ];
    for (msg, expected) in cases {
        assert_eq!(&triage_message(msg), expected, "triage of: {msg}");
    }
}

/// The class the issue is really about: reads that were becoming cards.
#[test]
fn read_requests_are_answers() {
    for msg in [
        "list the tasks",
        "show me the board",
        "what's our revenue?",
        "Tell what is there in the tasks list",
        "explain how the newsletter works",
        "describe the current backlog",
        "walk me through the funnel",
        "can you list the tasks?",
        "where do we stand on the launch?",
    ] {
        assert_eq!(
            triage_message(msg),
            MessageTriage::Answer,
            "should be an answer: {msg}"
        );
        assert!(detect_task_intent(msg).is_none(), "no card for: {msg}");
    }
}

/// The class the original suite never probed: for every lead-word pattern
/// it pinned, it picked the *read*-flavoured instance ("give me the
/// headcount", "walk me through the funnel", "describe the current
/// backlog") — so the work-flavoured instance of the same pattern went
/// unmeasured, and every one of these was [`MessageTriage::Answer`], which
/// withdraws the board tools from a request to build something (issue #267
/// review, finding 1).
///
/// The assertion is `!= Answer` rather than `== Track` on purpose. What
/// costs the operator is the gate; whether one of these lands in `Track` or
/// in the `Chatter` middle is the classifier's own tie-break and not what
/// this pins.
#[test]
fn a_work_ask_behind_a_read_lead_is_not_gated() {
    for msg in [
        // `give me` no longer leads READ_PHRASES.
        "give me a landing page",
        // …and a lead word no longer speaks for a conjoined imperative.
        "explain the auth flow and then fix the login bug",
        "compare our pricing to competitors and write up a doc",
        "walk me through the funnel and build a dashboard for it",
        "tell me the headcount; then draft the hiring plan",
        "show me the board. create a card for the launch",
    ] {
        assert_ne!(
            triage_message(msg),
            MessageTriage::Answer,
            "a request to produce something must not be gated: {msg}"
        );
    }
}

/// The other side of the same trade, and the reason the veto is scoped to
/// *later* clauses rather than applied as a blanket `!contains_action`:
/// [`ACTION_VERBS`] is full of words that are commonly nouns, so a blanket
/// veto would degrade these to `Chatter` and gut the layer.
#[test]
fn a_noun_that_doubles_as_an_action_verb_stays_a_question() {
    for msg in [
        "what's the status of the design review?",
        "who is running the security audit?",
        "when is the next product review and the board update?",
        "what's our revenue and how did it change?",
        "is the campaign live and is the newsletter out?",
    ] {
        assert_eq!(
            triage_message(msg),
            MessageTriage::Answer,
            "should stay an answer: {msg}"
        );
    }
}

/// Dropping `give me` from [`READ_PHRASES`] moves the read-flavoured
/// instance to `Chatter`, which is the whole cost of the change: no card,
/// no gate, so the operator loses nothing.
#[test]
fn a_read_flavoured_give_me_falls_to_the_safe_middle() {
    assert_eq!(
        triage_message("give me the headcount"),
        MessageTriage::Chatter
    );
    assert!(detect_task_intent("give me the headcount").is_none());
}

/// A request frame is read before any question test, so a politely phrased
/// instruction stays work even when it ends in `?`.
#[test]
fn a_request_frame_beats_an_interrogative() {
    assert_eq!(
        triage_message("can you build the landing page?"),
        MessageTriage::Track("Build the landing page".to_string())
    );
    assert_eq!(
        triage_message("could you please fix the checkout bug?"),
        MessageTriage::Track("Fix the checkout bug".to_string())
    );
}

/// The predicate the board guard turns on: deixis fires only when it is
/// the object of the request (see
/// [`board_deixis_must_be_the_objects_head_not_a_modifier_or_topic`] for
/// the cases that must NOT fire), field nouns demote only in board
/// context, and everything else is real work.
#[test]
fn board_entity_predicate_reads_the_object_of_the_request() {
    // Tier 1 — deixis, matched wherever it is the object of the request.
    for msg in [
        "update the status on the task card",
        "move this card to done",
        "close the ticket",
        "reprioritise the backlog",
        "look at the board",
    ] {
        assert!(refers_to_board_entity(msg), "should be board: {msg}");
    }
    // Tier 2 — a field noun that is the object of a board operation.
    assert!(refers_to_board_entity("update the status on the board"));
    assert!(refers_to_board_entity("bump the priority")); // clause-final
    assert!(refers_to_board_entity("change the assignee to nova")); // connective
    // Tier 2 — a field noun that heads a longer noun is real work.
    assert!(!refers_to_board_entity("update the status page"));
    assert!(!refers_to_board_entity("draft the priority list"));
    // No board vocabulary at all.
    for msg in [
        "update the landing page",
        "move the deploy to staging",
        "create a task tracker",
    ] {
        assert!(!refers_to_board_entity(msg), "should not be board: {msg}");
    }
    // A boundary check: deixis must not fire inside a larger word.
    assert!(!refers_to_board_entity("restock the cardstock"));
}

/// PR #1949 review (Codex thread 3895066476, CodeRabbit thread
/// 3895107555): `BOARD_DEIXIS` used to match anywhere in the message, so
/// a deliverable whose title merely *contains* board vocabulary — as a
/// compound noun, or as the topic of a different object — got misread as
/// the object of a board operation and demoted to `Chatter`, opening no
/// card. The predicate must require the deixis phrase to actually be the
/// object of the request, the same way [`field_noun_in_board_context`]
/// already requires for field nouns.
#[test]
fn board_deixis_must_be_the_objects_head_not_a_modifier_or_topic() {
    // "the board"/"the ticket" heads a longer noun ("board presentation",
    // "ticket booking flow") — real work, not a board operation.
    assert!(!refers_to_board_entity("build the board presentation"));
    assert!(!refers_to_board_entity("update the ticket booking flow"));
    // "about"/"regarding" make the deixis phrase the *topic* of a
    // different object ("a report"), not the object itself.
    assert!(!refers_to_board_entity("create a report about the board"));
    assert!(!refers_to_board_entity("write a memo regarding the board"));
    // Genuine deixis-as-object still fires — the verb's own complement
    // preposition ("at", "to") is not a topic marker.
    assert!(refers_to_board_entity("look at the board"));
    assert!(refers_to_board_entity("move this card to done"));
}

/// PR #1949 review (CodeRabbit thread 3895107555): `field_noun_in_board_
/// context` treated a trailing `of` exactly like `on`/`to`/`for`/`and`,
/// but `of` introduces the noun a field *belongs to* ("the status **of**
/// the landing page" = the landing page's status), not a board
/// operation's target value the way "change the assignee **to** nova"
/// does. Demoting real deliverable work phrased with `of` closed no card.
#[test]
fn field_noun_followed_by_of_is_not_board_context() {
    assert!(!refers_to_board_entity(
        "update the status of the landing page"
    ));
    // The other connectives are unaffected.
    assert!(refers_to_board_entity("update the status on the board"));
    assert!(refers_to_board_entity("change the assignee to nova"));
}

/// A board operation phrased as an instruction is a *decision* to touch the
/// existing card, not a new deliverable — so it is `Chatter` (Matched), not
/// a second `Track` card. The incident that opened the issue leads the list.
#[test]
fn a_board_operation_does_not_mint_a_second_card() {
    for msg in [
        "can you also update the status on the task card?",
        "update the status on the task card",
        "please move the card to done",
        "can you update the priority on this task?",
        "close the ticket",
        "update the status on the board",
    ] {
        let out = triage_message_detailed(msg);
        assert_eq!(out.triage, MessageTriage::Chatter, "should not card: {msg}");
        assert_eq!(
            out.confidence,
            TriageConfidence::Matched,
            "a board op is a decision, not an abstention: {msg}"
        );
        assert!(detect_task_intent(msg).is_none(), "no card for: {msg}");
    }
}

/// The other side of the trade: real work that merely mentions a board word
/// (or a field noun heading a longer noun) still cards.
#[test]
fn real_work_that_mentions_a_field_still_cards() {
    for (msg, title) in [
        ("update the landing page", "Update the landing page"),
        ("move the deploy to staging", "Move the deploy to staging"),
        ("update the status page", "Update the status page"),
        ("create a task tracker", "Create a task tracker"),
    ] {
        assert_eq!(
            triage_message(msg),
            MessageTriage::Track(title.to_string()),
            "should stay work: {msg}"
        );
    }
    assert_eq!(
        triage_message("can you review the design"),
        MessageTriage::Track("Review the design".to_string())
    );
}

/// Neither work nor a question: the safe middle that cards nothing and
/// gates nothing.
#[test]
fn neutral_chatter_is_chatter() {
    for msg in [
        "hi",
        "thanks",
        "the deck looks good to me",
        "i'll be offline tomorrow",
        "nice work on the launch",
        "…",
    ] {
        assert_eq!(
            triage_message(msg),
            MessageTriage::Chatter,
            "should be chatter: {msg}"
        );
    }
}

// ── Issue #678: which Chatter is a decision, and which is a shrug ───────

/// The whole point of the seam. `Chatter` covers two unlike things, and only
/// one of them is worth a second opinion.
#[test]
fn a_recognised_chatter_is_a_decision_and_the_residue_is_an_abstention() {
    for decided in ["", "   ", "hi", "hello", "thanks"] {
        let out = triage_message_detailed(decided);
        assert_eq!(out.triage, MessageTriage::Chatter, "{decided:?}");
        assert!(
            !out.abstained(),
            "a greeting or an empty message is recognised, not fallen back to: {decided:?}"
        );
    }
    for residue in [
        "the deck looks good to me",
        "i'll be offline tomorrow",
        "nice work on the launch",
    ] {
        let out = triage_message_detailed(residue);
        assert_eq!(out.triage, MessageTriage::Chatter, "{residue:?}");
        assert!(
            out.abstained(),
            "no rule matched this, so the Chatter is a shrug: {residue:?}"
        );
    }
}

/// Every arm that fires a rule reports `Matched` — an abstention must never
/// be reachable from a positive classification, or the escalation trigger
/// would spend a model call on messages the cheap layer already named.
#[test]
fn every_positive_classification_reports_matched() {
    for msg in [
        "draft the launch plan for next quarter",
        "can you build the landing page?",
        "what is on the board?",
        "show me the headcount",
        "create a workflow named nightly digest",
    ] {
        let out = triage_message_detailed(msg);
        assert!(
            !out.abstained(),
            "a rule decided this, so it is not an abstention: {msg:?} -> {:?}",
            out.triage
        );
        assert_ne!(
            out.triage,
            MessageTriage::Chatter,
            "fixture must exercise a non-Chatter arm: {msg:?}"
        );
    }
}

/// The seam is observational. `triage_message` is the byte-for-byte answer
/// it always was — #463 pins two card paths to the title it returns, so a
/// classification drift here would desynchronise the REST handler from
/// `chat_handler_card` and orphan the card.
#[test]
fn the_detailed_entry_point_changes_no_classification() {
    for msg in [
        "",
        "   ",
        "hi",
        "thanks",
        "…",
        "the deck looks good to me",
        "i'll be offline tomorrow",
        "draft the launch plan for next quarter",
        "can you build the landing page?",
        "what is on the board?",
        "show me the headcount",
        "ok now also draft the brief",
        "is the build ok?",
        "create a workflow named nightly digest",
    ] {
        assert_eq!(
            triage_message(msg),
            triage_message_detailed(msg).triage,
            "the detailed entry point must not reclassify: {msg:?}"
        );
    }
}

/// The `Track` arm still carries the exact string the REST handler writes,
/// which `chat_handler_card` re-derives to find that card (issue #463).
#[test]
fn track_titles_stay_byte_identical_to_detect_task_intent() {
    for msg in [
        "Build the landing page",
        "please can you fix the login bug!",
        "go ahead and publish the blog post",
        "ok now build the dashboard",
        "Can you build the landing page?",
    ] {
        assert_eq!(
            triage_message(msg).title().map(str::to_string),
            detect_task_intent(msg),
            "title contract for: {msg}"
        );
    }
}

#[test]
fn greetings_and_acks_do_not_fire() {
    for msg in [
        "hi",
        "Hello!",
        "hey",
        "thanks",
        "thank you",
        "ok",
        "okay",
        "cool",
        "got it",
        "sounds good",
        "perfect",
        "yes",
        "no",
        "sure",
        "done",
    ] {
        assert!(detect_task_intent(msg).is_none(), "should not fire: {msg}");
    }
}

#[test]
fn title_strips_frame_caps_and_trims() {
    assert_eq!(
        detect_task_intent("please can you fix the login bug!").as_deref(),
        Some("Fix the login bug")
    );
    assert_eq!(
        detect_task_intent("go ahead and publish the blog post").as_deref(),
        Some("Publish the blog post")
    );
}

#[test]
fn title_is_bounded() {
    let long = format!("build {}", "a very detailed feature ".repeat(20));
    let title = detect_task_intent(&long).expect("actionable");
    assert!(title.chars().count() <= TITLE_MAX + 1, "bounded: {title}");
}

#[test]
fn empty_and_whitespace_do_not_fire() {
    assert!(detect_task_intent("").is_none());
    assert!(detect_task_intent("   ").is_none());
}

#[test]
fn ack_prefix_then_request_still_fires() {
    // "ok" as a whole message is an ack, but not as a prefix of a real ask.
    assert!(detect_task_intent("ok now build the dashboard").is_some());
}

// ── Issue #1725: the small-talk fast path's own classifier ──

/// The reported message. "hi" is a pleasantry, in every spelling and
/// whatever punctuation and case it arrives in.
#[test]
fn a_bare_greeting_is_small_talk() {
    for text in ["hi", "Hi", "  hi  ", "hi!", "Hello.", "hey", "Good morning"] {
        assert_eq!(
            small_talk(text),
            Some(SmallTalk::Hello),
            "{text:?} should be a greeting"
        );
    }
    for text in ["thanks", "Thank you!", "cheers", "ty"] {
        assert_eq!(
            small_talk(text),
            Some(SmallTalk::Thanks),
            "{text:?} should be thanks"
        );
    }
}

/// The narrowing that keeps the fast path honest. An acknowledgement is
/// small talk on its own and an *instruction* in a conversation — "yes"
/// answering "shall I ship it?" must reach the turn that asked.
#[test]
fn an_acknowledgement_is_not_small_talk() {
    for text in [
        "yes", "no", "sure", "ok", "okay", "done", "lgtm", "got it", "nvm", "cool",
    ] {
        assert_eq!(small_talk(text), None, "{text:?} must still run a turn");
    }
}

/// A greeting with an ask under it is an ask. The fast path matches the
/// whole message for exactly this reason.
#[test]
fn a_greeting_with_a_request_under_it_is_not_small_talk() {
    for text in [
        "hi, build the landing page",
        "hey can you check the numbers?",
        "thanks — now ship it",
        "hi there team",
    ] {
        assert_eq!(small_talk(text), None, "{text:?} must still run a turn");
    }
}

/// Nothing said is not a pleasantry: there is no one to greet back.
#[test]
fn an_empty_message_is_not_small_talk() {
    assert_eq!(small_talk(""), None);
    assert_eq!(small_talk("   "), None);
    assert_eq!(small_talk("..."), None);
}

/// The subset invariant the fast path rests on: everything it answers is
/// already `Chatter`, so it can never take a card away from a message that
/// was getting one.
#[test]
fn every_pleasantry_is_also_a_greeting() {
    for word in HELLOS.iter().chain(THANKS.iter()) {
        assert!(
            GREETINGS.contains(word),
            "{word:?} is answered by the fast path but is not in GREETINGS"
        );
        assert_eq!(
            triage_message(word),
            MessageTriage::Chatter,
            "{word:?} must triage as Chatter"
        );
        assert!(
            !triage_message_detailed(word).abstained(),
            "{word:?} must be a decision, not an abstention"
        );
    }
}

/// The canned replies say nothing that can go stale.
#[test]
fn the_canned_replies_are_short_and_claim_nothing() {
    for talk in [SmallTalk::Hello, SmallTalk::Thanks] {
        let reply = talk.reply();
        assert!(!reply.trim().is_empty());
        assert!(reply.chars().count() <= 80, "{reply:?} is too long");
    }
}
