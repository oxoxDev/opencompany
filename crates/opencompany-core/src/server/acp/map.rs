//! Turning what this host streams into ACP `session/update` notifications.
//!
//! ## Why this reads the bus and not `AgentProgress`
//!
//! The obvious source is OpenHuman's own `AgentProgress`, which carries far
//! more. Two reasons it is the wrong one:
//!
//! - It only exists under `feature = "openhuman"`, and this surface has to
//!   compile in the default build. A host without the vendored runtime still
//!   serves ACP; it simply has less to say.
//! - [`steps::stream_event_from`](crate::harness::steps) is the **scrubbing
//!   boundary**. It is where tool arguments are redacted, where a remote body
//!   is reduced to a shape summary, and where a failure becomes a typed cause.
//!   Reading `AgentProgress` directly to get richer updates would route around
//!   all of that, and the richness would be exactly the raw material it exists
//!   to withhold.
//!
//! So the input here is [`TurnStreamEvent`] — already scrubbed — plus the
//! durable [`CompanyEvent`] journal.
//!
//! ## What is lost, said plainly
//!
//! `stream_event_from` returns `None` for `TextDelta`, so **there is no
//! incremental assistant text on this host**. An ACP client receives one
//! `agent_message_chunk` at the end of the turn, from the durable `AgentReply`.
//! Clients that render token-by-token will look like they have stalled and then
//! finished at once.
//!
//! That is a deliberate posture, not an oversight: `turn_stream`'s own module
//! documentation argues that the bus carries scrubbed projections only. Adding
//! deltas is a change to what this host is willing to stream, and belongs
//! behind a per-company decision rather than in a mapping layer.

use serde_json::{Value, json};

use crate::ports::types::CompanyEvent;
use crate::turn_stream::TurnStreamEvent;

/// One ACP `SessionUpdate`, ready to be wrapped in a `session/update`.
pub type SessionUpdate = Value;

/// Wraps an update in the notification envelope ACP expects.
pub fn notification(session_id: &str, update: SessionUpdate) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": { "sessionId": session_id, "update": update },
    })
}

/// Maps a live turn frame, or `None` when it has no ACP equivalent.
pub fn from_turn_stream(event: &TurnStreamEvent) -> Option<SessionUpdate> {
    match event.kind {
        "tool_call" => Some(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": event.tool_call_id.clone().unwrap_or_default(),
            "title": event.label.clone().unwrap_or_else(|| "Working".to_string()),
            "status": "pending",
            // `rawInput` is deliberately absent. `detail` is a *redacted*
            // one-liner about the call, not its arguments, and putting it in
            // ACP's raw-arguments field would tell a client it had the real
            // ones — which is how a scrubbed value ends up rendered as truth.
            "_meta": meta(event),
        })),
        "tool_result" => {
            let status = match event.status {
                Some("ok") => "completed",
                Some("error") => "failed",
                // A parked call is neither: the turn stopped, and nothing
                // broke. ACP has no such status, so it maps to `failed` with
                // the truth in `_meta` — see below.
                Some("awaiting_approval") => "failed",
                _ => "in_progress",
            };
            let mut update = json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": event.tool_call_id.clone().unwrap_or_default(),
                "status": status,
                "_meta": meta(event),
            });
            if let Some(result) = &event.result {
                update["content"] = json!([{
                    "type": "content",
                    "content": { "type": "text", "text": result },
                }]);
            }
            Some(update)
        }
        // A marker, with no content: this host never streams reasoning text.
        // Emitting an empty `agent_thought_chunk` would have a client render a
        // blank bubble on every turn, so it is dropped instead.
        "thinking" => None,
        _ => None,
    }
}

/// The OpenCompany-specific facts ACP has no field for.
///
/// `_meta` is the protocol's own escape hatch, and using it is how a
/// conforming client stays unaffected while ours can render the truth — most
/// importantly that a call is **parked on an approval** rather than failed.
fn meta(event: &TurnStreamEvent) -> Value {
    let mut meta = json!({ "opencompany/seq": event.seq });
    if let Some(detail) = &event.detail {
        meta["opencompany/detail"] = json!(detail);
    }
    if event.status == Some("awaiting_approval") {
        // The distinction ACP's four statuses cannot carry, and the one an
        // operator can act on. Reporting it as a plain failure would render the
        // single actionable state in the timeline as a crash.
        meta["opencompany/awaitingApproval"] = json!(true);
    }
    if event.truncated {
        meta["opencompany/truncated"] = json!(true);
    }
    meta
}

/// Maps a durable journal event, or `None` when it is not part of a session.
pub fn from_company_event(event: &CompanyEvent, chat: &str) -> Option<SessionUpdate> {
    match event {
        // The only place a reply's *text* appears — see the module docs.
        CompanyEvent::AgentReply { chat_id, text, .. } if chat_id == chat => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": text },
        })),
        // Echoed so a second client on the same desk sees what the first said.
        // Genuinely useful rather than incidental: two consoles on one thread is
        // the normal shape once a desktop and a browser are both connected.
        // `chat` is optional on an operator message: a send with no desk names
        // the default one, which is the same thread a session opened without a
        // desk is bound to.
        CompanyEvent::OperatorMessage { chat: c, text, .. }
            if c.as_deref()
                .unwrap_or(crate::server::ops::language::GENERAL_CHANNEL_ID)
                == chat =>
        {
            Some(json!({
                "sessionUpdate": "user_message_chunk",
                "content": { "type": "text", "text": text },
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "map_tests.rs"]
mod tests;
