//! Task-aware extraction of an oversized tool result (issue #6014).
//!
//! # What this replaces
//!
//! A tool result larger than the per-result budget used to be **cut on a byte
//! boundary**. That keeps the first few whole records and discards every one
//! after them, which is the wrong end to lose: a listing of thirty issues
//! became two, and the agent — correctly reporting what it could see — told the
//! operator there were two.
//!
//! Two cheaper transforms were tried first and are worth recording so nobody
//! re-derives them. **Dropping navigation fields**
//! ([`project_records_value`](crate::harness::composio_catalog::project_records_value))
//! measured **1.6x** on a real 237 KB GitHub payload — real, free, and nowhere
//! near enough. **Capping every long string** would have kept all thirty
//! records, but it clips the one body the operator asked about exactly as hard
//! as the twenty-nine they did not, because it has no idea which is which.
//!
//! The size is not the problem. The problem is that **nothing in the pipeline
//! knew what the turn was for**, so every strategy had to guess uniformly.
//!
//! # What it does instead
//!
//! [`PayloadSummarizer::maybe_summarize_in_parent`] is handed the tool name,
//! the raw payload **and the turn's task hint**, so the compression can keep
//! what answers the question and drop what does not. That trait has existed
//! upstream all along; OpenCompany simply never set it
//! (`AgentBuilder::payload_summarizer` defaults to `None`, and the only wiring
//! site is OpenHuman's own factory, which builds a **sub-agent** implementation
//! this crate cannot use — see `toolbelt`'s v1 note on why spawn tools are
//! withheld under multi-tenancy).
//!
//! So this is the same contract with a different engine: **one bounded,
//! tool-less model call**, the shape
//! [`TriageEvaluator`](crate::harness::triage), the card titler and
//! [`TaskPlanner`](crate::harness::planning) already use — built `from_deps`
//! so it shares the company's provider, its BYOK switch and its metering
//! rather than resolving a second credential path.
//!
//! # Failure is never fatal
//!
//! Every failure path returns [`SummarizeOutcome::Unavailable`], never `Err`:
//! a summarizer that times out must leave the turn holding the raw payload
//! (which downstream truncation still bounds), not kill the tool call. The
//! reason rides along so the model is told the result is unsummarized rather
//! than being handed a fragment silently.

use std::sync::Arc;
use std::time::Duration;

use openhuman_core as oh;

use oh::agent::tinyagents::payload_summarizer::PayloadSummarizer;
use oh::inference::tokenjuice::generate::{GenerateRequest, PreparedGenerate};

use crate::harness::HarnessDeps;
use crate::harness::build::model_for_tier;

/// How long one extraction may take before the turn gives up on it.
///
/// Deliberately short. This call sits **between a tool returning and the model
/// seeing its result**, so every millisecond is latency the operator watches
/// accumulate mid-turn. A slow extraction is worse than none: the raw payload
/// is still bounded downstream, so the fallback is the behaviour that shipped
/// before this existed.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(25);

/// Ceiling on the extraction's own output, so a summarizer cannot answer a
/// budget problem with a second one.
const MAX_SUMMARY_TOKENS: u32 = 1_500;

/// The most of an oversized payload that is worth sending to the extractor.
///
/// `raw` arrives here *because* it exceeded the per-result budget, so it has no
/// upper bound of its own — a multi-megabyte result would be sent, and billed,
/// in full, once per oversized tool result, and a payload past the model's
/// context window fails the call outright and spends the latency to return
/// `Unavailable` (CodeRabbit on tinyhumansai/opencompany#2153).
///
/// 256 KiB is far above the per-result budget that got us here and far below
/// any context window this runs against, so the ceiling only ever bites the
/// pathological case. The head of a payload is the right part to keep: record
/// arrays lead with records, and the archetype prompt asks for the count and
/// the page boundaries, which the head carries.
const MAX_EXTRACT_INPUT_CHARS: usize = 256 * 1024;

/// Cut `raw` to the input ceiling **on a character boundary**.
///
/// Slicing a `&str` by byte index panics mid-character, and provider payloads
/// carry plenty of multi-byte text (issue titles, names, emoji). Returns the
/// text to send and whether it was cut, so the prompt can say so — an extractor
/// told it is reading a prefix will not claim the payload ended there.
fn cap_input(raw: &str) -> (&str, bool) {
    if raw.len() <= MAX_EXTRACT_INPUT_CHARS {
        return (raw, false);
    }
    let mut end = MAX_EXTRACT_INPUT_CHARS;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    (&raw[..end], true)
}

/// One bounded model call that pulls the answering content out of an oversized
/// tool result. See the module docs.
pub struct PayloadExtractor {
    model: Arc<dyn tinyinference::model::ChatModel<()>>,
    model_name: String,
    /// What the extraction's spend is charged to.
    ///
    /// Carried rather than resolved here, for the reason every other one-shot
    /// pass in this crate carries it: the call spends the company's own
    /// credential, so it belongs on the company's ledger. Shipping without this
    /// made a per-oversized-result model call — whose input is the payload —
    /// invisible to the usage ledger and the per-turn spend accounting
    /// (codex on tinyhumansai/opencompany#2153).
    metering: Option<ExtractionMetering>,
}

/// The handles [`crate::metering::record_extraction_usage`] needs.
#[derive(Clone)]
struct ExtractionMetering {
    company: crate::ports::types::CompanyId,
    provider_slug: String,
    store: Arc<dyn crate::ports::CompanyStore>,
    meter: Option<Arc<dyn crate::ports::usage::UsageMeter>>,
    /// Read off the `HarnessModel` at construction: the extractor itself holds
    /// only a `ChatModel<()>`, which cannot name itself for telemetry.
    model_slug: Option<crate::metering::ModelSlug>,
}

impl PayloadExtractor {
    /// Built from the same deps the roster is, so the extraction spends the
    /// company's own credential and is metered against it — the reason every
    /// other one-shot pass in this crate is constructed this way rather than
    /// resolving its own.
    ///
    /// `agent_pin` is the calling agent's own `{provider, model}` pin, when
    /// the manifest names one — the same [`HarnessModel`](crate::harness::provider::HarnessModel)
    /// [`build_agent_with_model`](crate::harness::build::build_agent_with_model)
    /// resolved for that agent's primary turn. Round-2 review (comment
    /// 4012457329, keys rework issue #2306, X12): before this an oversized
    /// tool result was always extracted through the unpinned company default,
    /// so a company configured solely through agent pins — no default at all
    /// — lost extraction outright rather than falling back to the pin that
    /// was sitting right there. Resolved through
    /// [`pass_model`](crate::harness::built_in::pass_model): the default is
    /// still tried first on every call, and the pin is only reached when the
    /// default's own call fails.
    pub fn from_deps(
        deps: &HarnessDeps,
        company: &crate::ports::types::CompanyId,
        agent_pin: Option<Arc<dyn crate::harness::provider::HarnessModel>>,
    ) -> Self {
        let model_name = deps
            .model_override
            .clone()
            .unwrap_or_else(|| model_for_tier(None));
        Self {
            model: crate::harness::built_in::pass_model(deps, agent_pin)
                as Arc<dyn tinyinference::model::ChatModel<()>>,
            model_name,
            metering: Some(ExtractionMetering {
                company: company.clone(),
                provider_slug: deps.provider_slug.clone(),
                store: deps.store.clone(),
                meter: deps.meter.clone(),
                model_slug: deps.provider.telemetry_model(),
            }),
        }
    }
}

/// Charge one summary's tokens to the company that paid for them.
///
/// A free function rather than a method because the prepared call owns its
/// handles: TinyJuice may run it after the extractor that built it has gone.
///
/// Best-effort by design, on the same rule the titling and selector paths
/// follow: the call has already happened and the turn is already carrying its
/// result, so a ledger hiccup must cost the accounting row rather than the
/// turn.
async fn record_usage(
    metering: Option<&ExtractionMetering>,
    response: &tinyinference::model::ModelResponse,
) {
    let Some(metering) = metering else {
        return;
    };
    let usage = usage_from(response);
    crate::metering::record_extraction_usage(
        &usage,
        &metering.provider_slug,
        metering.model_slug,
        &metering.company,
        metering.store.as_ref(),
        metering.meter.as_deref(),
    )
    .await;
}

/// The tokens and charged cost one response reports.
///
/// Reads the provider's charged amount out of response metadata the same way
/// the titling pass does, so a managed provider's real dollar figure is used in
/// preference to a local estimate.
fn usage_from(response: &tinyinference::model::ModelResponse) -> crate::ports::types::TokenUsage {
    let tokens = response.usage.unwrap_or_default();
    let cost_usd = response
        .raw
        .as_ref()
        .and_then(|raw| raw.pointer("/openhuman_usage_meta/charged_amount_usd"))
        .and_then(serde_json::Value::as_f64)
        .filter(|c| c.is_finite() && *c > 0.0)
        .unwrap_or(0.0);
    crate::ports::types::TokenUsage {
        input: tokens.input_tokens,
        output: tokens.output_tokens,
        cached_input: tokens.cache_read_tokens,
        cost_usd,
    }
}

impl PayloadSummarizer for PayloadExtractor {
    fn prepare(
        &self,
        _parent_ctx: &tinyagents_harness::context::RunContext<
            oh::agent::tinyagents::host::run_context::OpenHumanRunContext,
        >,
    ) -> anyhow::Result<PreparedGenerate> {
        let model = Arc::clone(&self.model);
        let model_name = self.model_name.clone();
        let metering = self.metering.clone();

        Ok(Box::new(move |request: GenerateRequest| {
            Box::pin(async move {
                // TinyJuice decides *whether* to summarise and writes *what* to
                // ask; this decides *who answers and who pays*. The `purpose`
                // is carried into the log so an operator can still tell which
                // stage spent the call.
                let body = cap_input(&request.prompt);
                let (body, was_cut) = body;
                let max_tokens = request.max_output_tokens.clamp(1, MAX_SUMMARY_TOKENS);
                let model_request = tinyinference::model::ModelRequest {
                    messages: vec![
                        tinyinference::message::Message::system(request.system),
                        tinyinference::message::Message::user(body),
                    ],
                    model: Some(model_name),
                    max_tokens: Some(max_tokens),
                    ..Default::default()
                };

                let response =
                    match tokio::time::timeout(EXTRACT_TIMEOUT, model.invoke(&(), model_request))
                        .await
                    {
                        Ok(Ok(response)) => response,
                        Ok(Err(error)) => {
                            // An `Err` here leaves the caller holding the raw
                            // payload, which downstream truncation still bounds.
                            // It must never take the turn down: the result is
                            // large, not broken.
                            tracing::warn!(
                                purpose = %request.purpose,
                                %error,
                                "[payload-extract] summary call failed; the raw payload stands"
                            );
                            return Err(anyhow::anyhow!("payload summary failed: {error}"));
                        }
                        Err(_) => {
                            tracing::warn!(
                                purpose = %request.purpose,
                                timeout_s = EXTRACT_TIMEOUT.as_secs(),
                                "[payload-extract] summary timed out; the raw payload stands"
                            );
                            return Err(anyhow::anyhow!("payload summary timed out"));
                        }
                    };

                // Before anything is decided about the answer: the tokens are
                // spent either way, and a summary the caller then discards cost
                // exactly as much as one it used.
                record_usage(metering.as_ref(), &response).await;

                let summary = response.text();
                if summary.trim().is_empty() {
                    return Err(anyhow::anyhow!("payload summary returned nothing"));
                }
                // A summary built from a prefix must say so, in the host's
                // words rather than the model's (codex on
                // tinyhumansai/opencompany#2153). Without it the turn holds a
                // confident summary of a payload it only partly read — an
                // incomplete list presented as a complete one, which is the
                // precise failure this exists to end.
                Ok(match was_cut {
                    true => format!(
                        "_Read the first {sent} bytes of this result; later records were not \
                         examined. The full output is on disk — see the stored-results list for \
                         its path, and read it directly if the answer is not below._\n\n{summary}",
                        sent = body.len(),
                    ),
                    false => summary,
                })
            })
        }))
    }
}

#[cfg(test)]
#[path = "payload_extract_tests.rs"]
mod tests;
