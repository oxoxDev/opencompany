//! A small in-process tool loop over a [`ChatModel`], for the host's own
//! auxiliary passes that carry a tool whose *side effect* is the result.
//!
//! The company agents run on the embedded OpenHuman runtime, whose tool set
//! is its own (plus MCP servers) — there is no seam for a `Tool` this crate
//! built (plan hive-desks, Phase 2). Most host-side passes never needed one:
//! title, triage, planning and the selector are single tool-less calls on
//! the [`HarnessModel`](crate::harness::provider::HarnessModel). The workflow
//! copilot is the exception: it *is* three in-process tools
//! (`list_effective_tools`, `check_workflow`, `propose_workflow`) whose
//! accepted proposal is the whole output, and it is a create-time draft, not
//! a company agent — no session to resume, no console frames, no memory.
//! Serving those three over MCP to a runtime agent just to get them called
//! would put a network hop and a bearer between a builder and its own
//! result. So the copilot runs here instead: the same model, the same
//! `Tool` impls, the loop OpenHuman's session host would have run, minus
//! everything a company teammate needs and a copilot does not.
//!
//! What the loop does: send the messages and the tool schemas, execute every
//! tool call the model returns (in order, in-process), append the results as
//! tool messages, repeat until the model answers without a tool call or the
//! iteration cap is reached. Usage is summed across calls.

use std::sync::Arc;

use tinyinference::message::Message;
use tinyinference::model::{ChatModel, ModelRequest, ModelResponse};
use tinyinference::tool::{ToolFormat, ToolSchema};
use tinytools::Tool;

use crate::harness::cost::TurnUsage;

/// How one loop ended.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopEnd {
    /// The model answered without calling a tool.
    Replied(String),
    /// The iteration cap was reached while the model was still calling tools;
    /// the text is whatever it said on the last iteration.
    HitCap(String),
}

/// The loop's result: how it ended, and the summed usage of every call.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopOutcome {
    /// The ending.
    pub end: LoopEnd,
    /// Every model call's usage, summed.
    pub usage: TurnUsage,
}

/// Runs the loop.
///
/// `model_name` is what every request carries (`ModelRequest::model`);
/// `max_iterations` bounds the number of model calls. The tools are offered
/// as native tool schemas; a model that narrates a call in prose instead of
/// returning one calls nothing, which is the same contract the previous
/// native-dispatch builder had.
pub async fn run(
    model: &Arc<dyn ChatModel<()>>,
    model_name: &str,
    system_prompt: &str,
    user: &str,
    tools: &[Box<dyn Tool>],
    max_iterations: usize,
) -> anyhow::Result<LoopOutcome> {
    let schemas: Vec<ToolSchema> = tools
        .iter()
        .map(|tool| ToolSchema {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters_schema(),
            format: ToolFormat::default(),
        })
        .collect();
    let mut messages = vec![Message::system(system_prompt), Message::user(user)];
    let mut usage = TurnUsage::default();
    let mut last_text = String::new();
    for _ in 0..max_iterations.max(1) {
        let request = ModelRequest {
            messages: messages.clone(),
            tools: schemas.clone(),
            model: Some(model_name.to_string()),
            ..ModelRequest::default()
        };
        let response: ModelResponse = model.invoke(&(), request).await?;
        if let Some(reported) = response.usage.as_ref().or(response.message.usage.as_ref()) {
            usage.input_tokens += reported.input_tokens;
            usage.output_tokens += reported.output_tokens;
            usage.cached_input_tokens += reported.cache_read_tokens;
        }
        if let Some(charged) = response
            .raw
            .as_ref()
            .and_then(|raw| {
                raw.pointer("/openhuman/billing/charged_amount_usd")
                    .or_else(|| raw.pointer("/openhuman_usage_meta/charged_amount_usd"))
            })
            .and_then(serde_json::Value::as_f64)
        {
            usage.cost_usd += charged;
        }
        last_text = response.text();
        let calls = response.message.tool_calls.clone();
        if calls.is_empty() {
            return Ok(LoopOutcome {
                end: LoopEnd::Replied(last_text),
                usage,
            });
        }
        messages.push(Message::Assistant(response.message.clone()));
        for call in calls {
            let result = match tools.iter().find(|tool| tool.name() == call.name) {
                Some(tool) => match tool.execute(call.arguments.clone()).await {
                    Ok(result) => result.output(),
                    Err(err) => format!("error: {err:#}"),
                },
                None => format!("error: unknown tool `{}`", call.name),
            };
            messages.push(Message::tool(call.id.clone(), result));
        }
    }
    Ok(LoopOutcome {
        end: LoopEnd::HitCap(last_text),
        usage,
    })
}

#[cfg(test)]
#[path = "host_loop_tests.rs"]
mod tests;
