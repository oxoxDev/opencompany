use super::*;
use openhuman_core as oh;

use tinyinference::Result as TaResult;
use tinyinference::model::{ChatModel, ModelRequest, ModelResponse};

/// What the model does when the extractor calls it.
enum Behaviour {
    Reply(&'static str),
    Fail,
    Hang,
}

struct Scripted(Behaviour);

#[async_trait::async_trait]
impl ChatModel<()> for Scripted {
    async fn invoke(&self, _state: &(), _request: ModelRequest) -> TaResult<ModelResponse> {
        match self.0 {
            Behaviour::Reply(text) => Ok(ModelResponse::assistant(text)),
            Behaviour::Fail => Err(tinyinference::Error::Model("provider exploded".into())),
            Behaviour::Hang => {
                // Longer than EXTRACT_TIMEOUT, so the timeout arm is the one
                // under test rather than a race.
                tokio::time::sleep(EXTRACT_TIMEOUT * 4).await;
                Ok(ModelResponse::assistant("too late"))
            }
        }
    }
}

fn extractor(behaviour: Behaviour) -> PayloadExtractor {
    PayloadExtractor {
        model: Arc::new(Scripted(behaviour)),
        model_name: "test-model".to_string(),
        // No company handles under test here: these cases are about which
        // `SummarizeOutcome` each branch yields. The metering path is
        // exercised where the ledger and meter live.
        metering: None,
    }
}

/// Drives one prepared call the way TinyJuice does: build it, then hand it the
/// request TinyJuice would have written.
///
/// The prompt is TinyJuice's now, so `raw` arrives as `GenerateRequest::prompt`
/// rather than as a payload this crate frames itself.
async fn run(behaviour: Behaviour, raw: &str) -> anyhow::Result<String> {
    let ctx = tinyagents_harness::context::RunContext::new(
        tinyagents_harness::context::RunConfig::new("payload-extract-test"),
        oh::agent::tinyagents::host::run_context::OpenHumanRunContext::default(),
    );
    let call = extractor(behaviour)
        .prepare(&ctx)
        .expect("preparing binds handles and cannot fail here");
    call(GenerateRequest {
        context_token: "test".to_string(),
        purpose: "tool_result".to_string(),
        system: "Summarise the payload.".to_string(),
        prompt: raw.to_string(),
        max_output_tokens: 512,
    })
    .await
}

fn big() -> String {
    // No size gate here any more: TinyJuice decides *whether* to summarise and
    // only then asks. What these cases are about is what this host does once
    // it has been asked.
    format!(
        "[{}]",
        vec![r#"{"number":1,"title":"a flaky test"}"#; 2_000].join(",")
    )
}

/// A provider failure must leave the caller holding the raw payload, which
/// downstream truncation still bounds. It must never take the turn down: the
/// result is large, not broken.
#[tokio::test]
async fn a_provider_error_leaves_the_raw_payload_standing() {
    let error = run(Behaviour::Fail, &big())
        .await
        .expect_err("a failed call is an error, not a summary");
    assert!(
        format!("{error}").contains("payload summary failed"),
        "{error}"
    );
}

#[tokio::test]
async fn a_hanging_provider_times_out_rather_than_stalling_the_turn() {
    let error = run(Behaviour::Hang, &big())
        .await
        .expect_err("a hung call is an error, not a summary");
    assert!(format!("{error}").contains("timed out"), "{error}");
}

/// Empty is a failure, not a summary: substituting it would replace the tool's
/// output with nothing at all.
#[tokio::test]
async fn an_empty_answer_is_treated_as_a_failure() {
    let error = run(Behaviour::Reply("   \n  "), &big())
        .await
        .expect_err("blank is not an answer");
    assert!(format!("{error}").contains("returned nothing"), "{error}");
}

/// The path that matters: a working model, and its answer carried back.
#[tokio::test]
async fn the_models_answer_is_what_is_returned() {
    let summary = run(Behaviour::Reply("400 issues; #1 a flaky test"), &big())
        .await
        .expect("a summary");
    assert!(summary.contains("400 issues"), "{summary}");
}

/// A summary built from a prefix must say so. The summary *replaces* the tool
/// output, so without this the turn holds a confident account of a payload the
/// model only partly read — an incomplete list presented as a complete one,
/// which is the failure this exists to end (codex on
/// tinyhumansai/opencompany#2153).
#[tokio::test]
async fn a_summary_from_a_truncated_payload_discloses_the_cut() {
    let huge = format!("[{}]", vec![r#"{"n":1}"#; 60_000].join(","));
    assert!(
        huge.len() > MAX_EXTRACT_INPUT_CHARS,
        "the fixture must exceed the ceiling or nothing is cut"
    );
    let summary = run(Behaviour::Reply("60000 records"), &huge)
        .await
        .expect("a summary");
    assert!(
        summary.contains("later records were not examined"),
        "the cut must be disclosed: {summary}"
    );
    assert!(
        summary.contains("full output is on disk"),
        "the recovery route must be named: {summary}"
    );
    assert!(summary.contains("60000 records"), "{summary}");

    // A payload under the ceiling carries no such notice.
    let small = run(
        Behaviour::Reply("two records"),
        &format!("[{}]", vec![r#"{"n":1}"#; 400].join(",")),
    )
    .await
    .expect("a summary");
    assert!(
        !small.contains("not examined"),
        "nothing was cut, so nothing is disclosed: {small}"
    );
}

/// The input ceiling exists so a multi-megabyte payload is neither billed in
/// full nor sent past a context window. Slicing must land on a character
/// boundary — a byte index inside a multi-byte character panics, and
/// provider payloads are full of them.
#[test]
fn the_input_ceiling_cuts_on_a_character_boundary() {
    let (kept, cut) = cap_input("short");
    assert_eq!(kept, "short");
    assert!(!cut, "a small payload is not cut");

    // Every character is 4 bytes, so a byte-index slice at the ceiling would
    // land mid-character unless the boundary walk works.
    let wide = "\u{1F600}".repeat(MAX_EXTRACT_INPUT_CHARS);
    let (kept, cut) = cap_input(&wide);
    assert!(cut, "an oversized payload is cut");
    assert!(kept.len() <= MAX_EXTRACT_INPUT_CHARS);
    assert!(
        wide.starts_with(kept),
        "the cut keeps the head of the payload"
    );
}
