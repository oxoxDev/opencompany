//! Route an `output` node's report to a person or a channel (issue #170).
//!
//! An `output` node is a workflow's terminal "report back". Before this module
//! existed it produced a value that surfaced only in the console's transient
//! run-result drawer — so a workflow could compute an owner summary and had no
//! way to send it anywhere. A node's
//! [`destination`](crate::company::WorkflowDestinationDef) closes that gap.
//!
//! # Where this runs, and why here
//!
//! Delivery is **host-side and post-engine**: [`deliver_outputs`] is called from
//! [`run_workflow_inner`](super::runner) once `tinyflows::engine::run` has
//! returned, NOT from the HTTP handler. Three callers drive the same
//! [`WorkflowRunner`](crate::ports::WorkflowRunner) port — the console's run
//! route, the orchestrator's `run_workflow` tool, and the trigger scheduler —
//! and a *scheduled* run is precisely the case where nobody is watching the
//! drawer. Putting delivery in the handler would give the console the only
//! working destination.
//!
//! The engine never sees a destination. That is why it is a first-class model
//! field rather than node `config`: config is lowered into the engine graph, and
//! an inert key riding into the engine is exactly what the reserved-key
//! validation exists to prevent.
//!
//! # The security boundary
//!
//! **No path here may let a workflow email an arbitrary external address without
//! an explicit company grant.** The three destination kinds are gated
//! differently because they carry different risk:
//!
//! * **`owner`** — recipients are resolved *server-side* from the company's own
//!   [`UserStore`](crate::ports::UserStore) (active `Admin` users). The graph
//!   names no address, so an author cannot point it at an outsider. Constrained
//!   by construction; no grant needed. With no admin address (or no mailbox) it
//!   falls back to the always-present `operator` channel rather than becoming a
//!   silent no-op.
//! * **`email`** — the graph names an arbitrary address, so it is the dangerous
//!   one and carries **two independent gates**, both fail-closed:
//!   1. the company's `[tools].allow` must cover the `email` namespace (the same
//!      [`grants_cover`](crate::harness::build::grants_cover) matcher that gates
//!      an agent's `email.send` effect and a workflow `tool_call`), and
//!   2. the recipient must be an **established thread** — the company's inbox
//!      must already hold inbound mail from that address. This is the rule
//!      ported verbatim from the agent send path
//!      ([`crate::runtime::cycle`]); a cold recipient is **skipped and
//!      reported**, never sent to.
//! * **`channel`** — the target must match a [`ChannelAdapter`] the deployment
//!   already wired. A graph cannot conjure a channel; it can only address one an
//!   operator installed. Constrained by construction, like `owner`.
//!
//! A cold `email` recipient is a real product gap, not a design end-state: the
//! agent path parks such a send for operator approval, but the engine's approval
//! pause has no resume path today, so gating delivery on it would dead-end the
//! report instead of delaying it. Skipping loudly is the honest behaviour until
//! a resume path exists.
//!
//! # Failure is reported, never fatal
//!
//! A delivery failure must not fail a run that already did its work. Every
//! attempt yields a [`DeliveryReport`] row on
//! [`WorkflowRun::deliveries`](crate::ports::WorkflowRun), which rides the run
//! response into the console's run-result panel — so an operator can tell a
//! delivered report from an undelivered one **without reading a log**. There is
//! one attempt per recipient and no retry: a workflow run is not a mail queue.
//!
//! An output node the run never reached (an untaken branch, or a path that
//! paused for approval) gets no attempt and no row — an absent row means "not
//! reached", never "silently dropped".

use std::sync::Arc;

use serde_json::Value;

use crate::company::WorkflowFile;
use crate::company::runtime::CompanyMail;
use crate::company::{WorkflowDestinationDef, WorkflowNodeKind};
use crate::ports::types::{CompanyId, CompanyRecord, OutboundMessage};
use crate::ports::{
    ChannelAdapter, DeliveryReport, DeliveryStatus, EmailRecord, InboxStore, UserRole, UserStatus,
    UserStore, generate_id, now_millis,
};
use crate::server::ops::mailer::{MailCredentials, OutboundEmail};
use crate::server::ops::smtp::local_part;

/// How much report text one delivery carries. A workflow can emit an arbitrarily
/// large payload; an email or chat message that large helps nobody and may be
/// refused by the transport, so the body is truncated (on a **character**
/// boundary — never a byte slice, which panics mid-codepoint) with a visible
/// marker so the reader knows the text was cut.
const MAX_REPORT_CHARS: usize = 16_000;

/// The marker appended when a report is truncated at [`MAX_REPORT_CHARS`].
const TRUNCATION_MARKER: &str = "\n\n… (report truncated)";

/// How many inbox messages the established-thread check scans. Mirrors the
/// agent send path in [`crate::runtime::cycle`] so the two cannot disagree about
/// what "established" means.
const ESTABLISHED_SCAN_LIMIT: usize = 500;

/// The ports an output destination needs, bundled so [`HarnessDeps`] grows one
/// optional field rather than four.
///
/// [`HarnessDeps::delivery`](crate::harness::HarnessDeps) is `Option<Self>` and
/// defaults to `None` at every construction site except the production runtime
/// builder. `None` **fails closed and loud**: [`deliver_outputs`] attempts
/// nothing and writes a `failed` row naming the gap, so an operator sees "this
/// build cannot deliver" in the run result instead of an authored destination
/// quietly doing nothing.
#[derive(Clone)]
pub struct WorkflowDeliveryDeps {
    /// The company's own outbound-mail handle (sender + its SMTP credentials).
    /// `None` when the company has no mailbox: `owner` then falls back to the
    /// operator channel, and `email` is reported `skipped`.
    pub mail: Option<CompanyMail>,
    /// The company's inboxes — both the established-thread check and the
    /// outbound audit record go through this port.
    pub inbox: Arc<dyn InboxStore>,
    /// The company's user directory: how an `owner` destination resolves to
    /// actual addresses, server-side.
    pub users: Arc<dyn UserStore>,
    /// Every wired channel adapter, including the always-present `operator`.
    pub channels: Vec<Arc<dyn ChannelAdapter>>,
}

impl std::fmt::Debug for WorkflowDeliveryDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the mail handle itself — `CompanyMail` carries `SmtpCredentials`,
        // whose derived `Debug` prints the password (see `mailer::test`).
        f.debug_struct("WorkflowDeliveryDeps")
            .field("mail", &self.mail.is_some())
            .field(
                "channels",
                &self
                    .channels
                    .iter()
                    .map(|c| c.channel_id().to_string())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Delivers every reached `output` node's report to its configured destination,
/// returning one [`DeliveryReport`] per attempt.
///
/// Never returns an error: a delivery problem is data on the run result, not a
/// failed run (see the module docs). Nodes with no `destination`, and nodes the
/// run never reached, produce no rows at all.
pub async fn deliver_outputs(
    delivery: Option<&WorkflowDeliveryDeps>,
    record: &CompanyRecord,
    workflow: &WorkflowFile,
    output: &Value,
) -> Vec<DeliveryReport> {
    let mut reports = Vec::new();

    for node in &workflow.nodes {
        // Only `output` nodes report back. Validation already rejects a
        // `destination` on any other kind; this is the belt to that braces, so a
        // graph loaded from an older/looser source can never deliver from, say,
        // an agent node.
        if node.kind != WorkflowNodeKind::Output {
            continue;
        }
        let Some(destination) = &node.destination else {
            continue;
        };
        // An output node the run never reached (untaken branch, or a path that
        // paused for approval) is not a delivery that failed — it is a delivery
        // that was never owed. No attempt, no row.
        if !node_was_reached(output, &node.id) {
            tracing::debug!(
                company = %record.id,
                workflow = %workflow.id,
                node = %node.id,
                "workflow delivery: output node not reached; nothing to deliver"
            );
            continue;
        }

        let text = report_text(output, &node.id);
        let subject = subject_for(record, workflow, &node.name);

        let Some(delivery) = delivery else {
            // The #169 lesson: a silent skip is indistinguishable from a working
            // destination. Say it where the operator actually looks — the run
            // result — and in the log.
            tracing::warn!(
                company = %record.id,
                workflow = %workflow.id,
                node = %node.id,
                kind = %destination.kind,
                "workflow delivery: this build has no delivery ports wired; the report was NOT sent"
            );
            reports.push(DeliveryReport {
                node: node.id.clone(),
                kind: destination.kind.clone(),
                target: destination.target.clone(),
                status: DeliveryStatus::Failed,
                detail: "report delivery is not wired on this runtime — the workflow ran and its \
                         result is in this run, but nothing was sent"
                    .to_string(),
            });
            continue;
        };

        deliver_one(
            delivery,
            record,
            &node.id,
            destination,
            &subject,
            &text,
            &mut reports,
        )
        .await;
    }

    reports
}

/// Dispatches one node's destination, appending every attempt's row to
/// `reports`. `owner` can fan out to several admins, so this appends rather than
/// returning a single report.
async fn deliver_one(
    delivery: &WorkflowDeliveryDeps,
    record: &CompanyRecord,
    node_id: &str,
    destination: &WorkflowDestinationDef,
    subject: &str,
    text: &str,
    reports: &mut Vec<DeliveryReport>,
) {
    let row = |target: Option<String>, status: DeliveryStatus, detail: String| DeliveryReport {
        node: node_id.to_string(),
        kind: destination.kind.clone(),
        target,
        status,
        detail,
    };
    let target = destination.target.as_deref().map(str::trim).unwrap_or("");

    match destination.kind.trim() {
        // --- owner: resolved server-side; the graph names nobody -------------
        "owner" => {
            let admins = admin_emails(delivery.users.as_ref(), &record.id).await;
            match (&delivery.mail, admins.is_empty()) {
                (Some(mail), false) => {
                    for address in admins {
                        let result =
                            send_email(delivery, mail, &record.id, &address, subject, text).await;
                        reports.push(match result {
                            Ok(()) => row(
                                Some(address),
                                DeliveryStatus::Sent,
                                "emailed the company's admin".to_string(),
                            ),
                            Err(err) => row(
                                Some(address),
                                DeliveryStatus::Failed,
                                format!("the mail transport refused the message: {err}"),
                            ),
                        });
                    }
                }
                // No mailbox, or no admin has an address: fall back to the
                // always-present operator channel so the owner still hears about
                // it. Never a silent no-op.
                _ => {
                    let why = if delivery.mail.is_none() {
                        "no mailbox is configured for this company"
                    } else {
                        "no active admin has an email address"
                    };
                    reports.push(
                        post_to_channel(
                            delivery,
                            crate::runtime::channel::OPERATOR_CHANNEL,
                            subject,
                            text,
                        )
                        .await
                        .map(|()| {
                            row(
                                Some(crate::runtime::channel::OPERATOR_CHANNEL.to_string()),
                                DeliveryStatus::Sent,
                                format!("{why}, so the report went to the operator channel"),
                            )
                        })
                        .unwrap_or_else(|detail| {
                            row(
                                Some(crate::runtime::channel::OPERATOR_CHANNEL.to_string()),
                                DeliveryStatus::Failed,
                                format!(
                                    "{why}, and the operator channel fallback failed: {detail}"
                                ),
                            )
                        }),
                    );
                }
            }
        }

        // --- email: the graph names an address, so it is double-gated --------
        "email" => {
            // GATE 1 — the company must grant the `email` namespace. Checked
            // FIRST and independently of whether mail is even wired, so a
            // missing grant is always reported as a denial rather than being
            // masked by an unrelated configuration gap.
            if !crate::harness::build::grants_cover(&record.manifest.tools.allow, "email") {
                tracing::warn!(
                    company = %record.id,
                    node = %node_id,
                    "workflow delivery: refused an email destination — the company does not grant `email`"
                );
                reports.push(row(
                    Some(target.to_string()),
                    DeliveryStatus::Denied,
                    "this company's [tools].allow does not grant `email`, so a workflow may not \
                     send mail to a named address"
                        .to_string(),
                ));
                return;
            }
            let Some(mail) = &delivery.mail else {
                reports.push(row(
                    Some(target.to_string()),
                    DeliveryStatus::Skipped,
                    "no mailbox is configured for this company, so there is nothing to send from"
                        .to_string(),
                ));
                return;
            };
            // GATE 2 — the established-thread rule, ported from the agent send
            // path. Fails closed: an inbox read error counts as "cold".
            if !recipient_is_established(
                delivery.inbox.as_ref(),
                &record.id,
                &mail.smtp.from_email,
                target,
            )
            .await
            {
                tracing::warn!(
                    company = %record.id,
                    node = %node_id,
                    "workflow delivery: skipped an email destination — the recipient is not an established thread"
                );
                reports.push(row(
                    Some(target.to_string()),
                    DeliveryStatus::Skipped,
                    "this recipient has never written to the company, so a workflow may not open \
                     the conversation — send once from the inbox first"
                        .to_string(),
                ));
                return;
            }
            reports.push(
                match send_email(delivery, mail, &record.id, target, subject, text).await {
                    Ok(()) => row(
                        Some(target.to_string()),
                        DeliveryStatus::Sent,
                        "emailed the named recipient on an established thread".to_string(),
                    ),
                    Err(err) => row(
                        Some(target.to_string()),
                        DeliveryStatus::Failed,
                        format!("the mail transport refused the message: {err}"),
                    ),
                },
            );
        }

        // --- channel: only a channel the deployment already wired ------------
        "channel" => {
            reports.push(
                match post_to_channel(delivery, target, subject, text).await {
                    Ok(()) => row(
                        Some(target.to_string()),
                        DeliveryStatus::Sent,
                        "posted to the channel".to_string(),
                    ),
                    Err(detail) => row(Some(target.to_string()), DeliveryStatus::Failed, detail),
                },
            );
        }

        // Unreachable through `parse_workflow`, which rejects an unknown kind.
        // Reported rather than ignored so a graph that somehow bypassed
        // validation cannot deliver nowhere in silence.
        other => reports.push(row(
            destination.target.clone(),
            DeliveryStatus::Failed,
            format!("`{other}` is not a destination kind this runtime knows how to deliver to"),
        )),
    }
}

/// The active admins' email addresses, in store order. An unreadable user store
/// yields none (which routes `owner` to the operator-channel fallback) rather
/// than failing the run.
async fn admin_emails(users: &dyn UserStore, company: &CompanyId) -> Vec<String> {
    match users.list_users(company).await {
        Ok(list) => list
            .into_iter()
            .filter(|u| u.role == UserRole::Admin && u.status == UserStatus::Active)
            .map(|u| u.email)
            .filter(|email| email.contains('@'))
            .collect(),
        Err(err) => {
            tracing::warn!(
                company = %company,
                error = %err,
                "workflow delivery: could not read the user directory; falling back to the operator channel"
            );
            Vec::new()
        }
    }
}

/// Sends one email through the company's own mail handle and mirrors it into the
/// company inbox as outbound (the same audit trail the agent send path and the
/// console's test-send leave, and what makes the thread "established" for a
/// later reply).
async fn send_email(
    delivery: &WorkflowDeliveryDeps,
    mail: &CompanyMail,
    company: &CompanyId,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), crate::error::OpenCompanyError> {
    let email = OutboundEmail {
        to: to.to_string(),
        subject: subject.to_string(),
        body: body.to_string(),
    };
    mail.sender
        .send(&MailCredentials::Smtp(mail.smtp.clone()), &email)
        .await?;
    record_outbound(delivery.inbox.as_ref(), company, mail, &email).await;
    Ok(())
}

/// Appends a sent email to the sending inbox so the console shows it alongside
/// inbound mail. Mirrors [`crate::server::ops::smtp::record_outbound`], which
/// takes a whole `CompanyRuntime` this path does not have.
async fn record_outbound(
    inbox: &dyn InboxStore,
    company: &CompanyId,
    mail: &CompanyMail,
    email: &OutboundEmail,
) {
    let record = EmailRecord {
        id: generate_id(),
        inbox: local_part(&mail.smtp.from_email),
        from_name: mail.smtp.from_name.clone(),
        from_email: mail.smtp.from_email.clone(),
        subject: email.subject.clone(),
        body: email.body.clone(),
        at_millis: now_millis(),
        read: true,
        outbound: true,
    };
    if let Err(err) = inbox.append(company, &record).await {
        tracing::warn!(
            company = %company,
            error = %err,
            "workflow delivery: failed to record the outbound email"
        );
    }
}

/// Whether the company's inbox already holds **inbound** mail from `to` — an
/// established thread.
///
/// Fails closed (`false`) on a missing sending address or an inbox read error,
/// which routes the caller to the cold-recipient skip. Byte-for-byte the same
/// rule as `recipient_is_established` in [`crate::runtime::cycle`].
async fn recipient_is_established(
    inbox: &dyn InboxStore,
    company: &CompanyId,
    company_address: &str,
    to: &str,
) -> bool {
    if company_address.trim().is_empty() {
        return false;
    }
    let key = local_part(company_address);
    let to = to.trim().to_ascii_lowercase();
    if to.is_empty() {
        return false;
    }
    match inbox
        .messages(company, &key, ESTABLISHED_SCAN_LIMIT, 0)
        .await
    {
        Ok(records) => records
            .iter()
            .any(|r| !r.outbound && r.from_email.trim().to_ascii_lowercase() == to),
        Err(_) => false, // fail closed → the cold-recipient skip
    }
}

/// Posts a report to the wired channel adapter with id `channel_id`.
///
/// `Err(detail)` carries an operator-readable reason: an unwired id names what
/// *is* wired, so the fix is obvious from the run result alone.
async fn post_to_channel(
    delivery: &WorkflowDeliveryDeps,
    channel_id: &str,
    subject: &str,
    text: &str,
) -> Result<(), String> {
    let Some(adapter) = delivery
        .channels
        .iter()
        .find(|c| c.channel_id() == channel_id)
    else {
        let wired: Vec<&str> = delivery
            .channels
            .iter()
            .map(|c| c.channel_id())
            .collect::<Vec<_>>();
        return Err(if wired.is_empty() {
            format!("`{channel_id}` is not wired on this runtime, which has no channels at all")
        } else {
            format!(
                "`{channel_id}` is not a wired channel — this runtime has: {}",
                wired.join(", ")
            )
        });
    };
    adapter
        .send(OutboundMessage {
            channel: channel_id.to_string(),
            text: format!("{subject}\n\n{text}"),
            steps: Vec::new(),
            reply_to: None,
        })
        .await
        .map_err(|err| format!("the channel refused the message: {err}"))
}

/// Whether the run's output carries an entry for `node_id` — i.e. the engine
/// actually reached that node.
fn node_was_reached(output: &Value, node_id: &str) -> bool {
    !output
        .get("nodes")
        .and_then(|nodes| nodes.get(node_id))
        .unwrap_or(&Value::Null)
        .is_null()
}

/// The report body for one output node: every item's text, in order.
///
/// The engine emits items as `{"json": {...}}`, and the `text` an agent node
/// produced sometimes sits one level deeper (`json.json.text`) — the same
/// double-wrapping the console's run-result parser handles, so the outer value
/// wins here too. An item carrying no readable text falls back to its compact
/// JSON, so a data-shaped report is delivered rather than dropped.
fn report_text(output: &Value, node_id: &str) -> String {
    let items = output
        .get("nodes")
        .and_then(|nodes| nodes.get(node_id))
        .and_then(|node| node.get("items"))
        .and_then(Value::as_array);
    let Some(items) = items else {
        return "(this workflow step produced no output)".to_string();
    };

    let mut parts: Vec<String> = Vec::new();
    for item in items {
        let json = item.get("json").unwrap_or(item);
        if let Some(text) = read_nested_str(json, "text") {
            parts.push(text.to_string());
        } else {
            parts.push(json.to_string());
        }
    }
    if parts.is_empty() {
        return "(this workflow step produced no output)".to_string();
    }
    truncate_chars(&parts.join("\n\n"), MAX_REPORT_CHARS)
}

/// Reads a string field from an item's `json`, preferring the outermost value
/// and falling back to the nested `json.json.<key>` the engine sometimes emits.
fn read_nested_str<'a>(json: &'a Value, key: &str) -> Option<&'a str> {
    let non_empty = |v: &'a Value| v.as_str().filter(|s| !s.trim().is_empty());
    if let Some(outer) = json.get(key).and_then(non_empty) {
        return Some(outer);
    }
    json.get("json")
        .and_then(|inner| inner.get(key))
        .and_then(non_empty)
}

/// Truncates `text` to at most `max` characters, appending a visible marker when
/// it actually cut something.
///
/// Character-indexed on purpose: slicing a `String` by byte offset panics when
/// the offset lands mid-codepoint, and a report can carry any UTF-8 the run
/// produced.
fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((byte_index, _)) => format!("{}{TRUNCATION_MARKER}", &text[..byte_index]),
    }
}

/// The subject line one report carries: the company, the workflow, and which
/// step reported.
fn subject_for(record: &CompanyRecord, workflow: &WorkflowFile, node_name: &str) -> String {
    format!(
        "[{}] {} — {}",
        record.manifest.company.name, workflow.name, node_name
    )
}
