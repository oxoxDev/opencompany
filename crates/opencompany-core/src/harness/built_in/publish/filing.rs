//! Filing a publish: the card it lands on and the artifact chain it extends.
//!
//! Moved off `HarnessBrain` because a brain is no longer the only thing that
//! finishes a turn. A hive episode's seat publishes inside a turn the brain
//! never sees -- the episode is spawned detached and runs on its own task --
//! so the code that records a deliverable cannot live on the one type that
//! happens to run chat turns.
//!
//! What it needed was never the brain. Five references, all of them `deps` or
//! the company id: [`PublishFiling`] is those two, and both callers hand them
//! over. The bodies are unchanged from the methods they replace.

use super::super::*;
use crate::company::artifact_mirror;
use crate::ports::artifacts::{ArtifactAuthor, ArtifactRecord};
use crate::ports::tasks::{COLUMN_IN_REVIEW, TaskOutputArtifact};
use crate::ports::{TaskOrigin, TaskRecord, generate_id, now_millis};
use crate::runtime::delegation::ChatTarget;

/// The stores a publish is filed into, for whoever is finishing the turn.
pub(crate) struct PublishFiling<'a> {
    /// The company the card and its artifacts belong to.
    pub(crate) company: &'a CompanyId,
    /// Where the task board and the artifact store are reached.
    pub(crate) deps: &'a HarnessDeps,
}

impl PublishFiling<'_> {
    /// Records everything the run published as versioned artifacts, returning
    /// one reference per artifact **pinned at the version this run wrote**
    /// (issues #244, #339).
    ///
    /// # Why the version comes back
    ///
    /// The caller stamps these onto the card, and a card link that named only
    /// the artifact would re-point at whatever a human last edited — silently
    /// turning "what this task produced" into "what the artifact says now".
    /// `push_version` already computes the number; before #339 it was
    /// discarded.
    ///
    /// # Extend by identity, never by recency
    ///
    /// The record to extend is the one on this card whose `source` equals the
    /// published path. That is the correction at the heart of this issue.
    ///
    /// The old rule was `max_by_key(updated_at_millis)` — extend whichever
    /// artifact on the card was touched most recently. An **operator edit**
    /// bumps `updated_at_millis`, so editing the invoice made the invoice the
    /// target for the next agent write to the spec: the spec's v3 landed as the
    /// invoice's v4, and `human_edit_diff` then reported an operator rewriting
    /// a document they had never seen. Since that diff is the entire purpose of
    /// the artifact port, recency did not merely mis-file records — it
    /// fabricated the one number the product exists to measure.
    ///
    /// A path that has never been published opens a new record; a rename starts
    /// a new lineage, which is a limitation named on
    /// [`ArtifactRecord::source`](crate::ports::artifacts::ArtifactRecord::source)
    /// rather than papered over with a guess.
    ///
    /// # Errors propagate — to the caller, which now contains them
    ///
    /// Deliberately, and this was a change in #244. The pre-#244 path returned
    /// a silent `Ok(())` when the store was missing and swallowed nothing else,
    /// which meant a failed write to a deliverable an agent had explicitly
    /// published was indistinguishable from success. An explicit publish that
    /// could not be stored is a real failure of the run and the operator needs
    /// to see it, so this still surfaces one.
    ///
    /// What changed in #339 is where that failure stops. This now runs
    /// **before** the card's single write, so `run_task` logs the error at
    /// `error` and settles the card anyway rather than propagating — a
    /// bookkeeping fault must not strand a finished card in `in_progress`. The
    /// error is still raised here; it is simply no longer fatal there.
    ///
    /// A **missing store** is different: `publish_artifact` is not wired at all
    /// without one (see `build.rs`), so a non-empty queue here means something
    /// upstream is misconfigured. It warns loudly rather than failing the cycle,
    /// because the turn's actual work is already done and persisted.
    ///
    /// `run_id` stamps the revision this call writes (#242) so a run row can
    /// point at what it actually produced. An earlier attempt's version keeps
    /// the attempt that wrote *it*.
    ///
    /// # Authorship is per file, not per call (issue #463)
    ///
    /// Each revision records the agent that published **that file**, read from
    /// [`PendingPublish::agent`]. `responder` is only the fallback, for a value
    /// built by hand rather than by the tool.
    ///
    /// One drain can hold publishes from more than one agent — the desk lead's
    /// turn and the orchestrator's own turn both run with the full toolbelt
    /// under a single `Conversation` claim — so a single author applied to the
    /// batch stamps one agent's name on another's file. The card above still
    /// takes one owner, because a card has one; a revision is a different
    /// question with a different answer.
    pub(crate) async fn record_published_artifacts(
        &self,
        card: &TaskRecord,
        responder: &str,
        published: Vec<publish::PendingPublish>,
        run_id: Option<&str>,
    ) -> crate::Result<Vec<TaskOutputArtifact>> {
        if published.is_empty() {
            // The honest, common case: this run produced no file. There is no
            // artifact, and the run trace is the addressable record of what
            // happened.
            return Ok(Vec::new());
        }
        let Some(artifacts) = self.deps.artifacts.as_ref() else {
            tracing::warn!(
                task_id = %card.id,
                staged = published.len(),
                "[publish] files were published but no artifact store is configured; the tool \
                 should not have been wired — nothing was recorded"
            );
            return Ok(Vec::new());
        };

        let mut on_card = artifacts.list(self.company, Some(&card.id)).await?;
        let mut written = Vec::with_capacity(published.len());
        for pending in published {
            let at = now_millis();
            // Issue #463: whoever published THIS file. `responder` is the
            // fallback for a `PendingPublish` not built by the tool — the tool
            // always stamps its own agent.
            let author = match pending.agent.trim() {
                "" => responder,
                agent => agent,
            };
            // Identity, not recency: the record whose `source` is this exact
            // path, or a new one.
            let existing = on_card
                .iter()
                .position(|a| a.source.as_deref() == Some(pending.source.as_str()));
            // The revision THIS run wrote (#339). A fresh record is always its
            // own v1; an extended one takes whatever `push_version` numbered.
            let mut version = 1;
            // The node the PREVIOUS version was mirrored into, read before the
            // push below appends a version whose own node is not chosen yet.
            let mut prior_node = None;
            let mut record = match existing {
                Some(index) => {
                    let mut found = on_card.remove(index);
                    prior_node = found.workspace_node_id().map(str::to_string);
                    version = found.push_version(
                        pending.payload.artifact_body(),
                        ArtifactAuthor::Agent,
                        author,
                        at,
                        pending.note.clone(),
                    );
                    // A republished file may have changed shape — a markdown
                    // draft exported as a PDF, a small file grown past the
                    // inline cap. The record follows what was actually
                    // captured, or the console renders the new version with the
                    // old version's renderer.
                    found.kind = pending.kind;
                    found
                }
                None => {
                    let mut fresh = ArtifactRecord::new(
                        generate_id(),
                        &card.id,
                        &pending.title,
                        pending.kind,
                        pending.payload.artifact_body(),
                        author,
                        at,
                    )
                    .with_source(pending.source.clone());
                    if let Some(note) = pending.note.clone()
                        && let Some(first) = fresh.versions.first_mut()
                    {
                        first.note = Some(note);
                    }
                    fresh
                }
            };
            if let Some(run_id) = run_id {
                record.stamp_run(run_id);
            }
            // Issue #552: the deliverable also goes into the shared workspace
            // tree, which is the one surface the operator browses and every
            // other agent can read. The artifact chain here stays the
            // authoritative version history; the node holds the current body.
            //
            // # Chain first, without exception
            //
            // A re-publish inherits the node the previous version named, so the
            // version can be written *before* the tree is touched. That
            // ordering is the load-bearing half of keeping the chain
            // authoritative, not a preference: a node one version ahead of the
            // chain is the tree showing content the version history has no
            // record of, which makes `human_edit_diff` quietly wrong rather
            // than loudly broken — the same #187 rot arriving by a different
            // door. Requiring the store to half-fail bounds how *often* that
            // happens and not at all how bad it is, and on a data path a silent
            // wrong answer outlives the incident that caused it.
            //
            // A *fresh* publish has no node id to inherit, so its v1 is stored
            // unlinked and the link is stamped by the second upsert below. Note
            // what that buys beyond ordering: because the record is written
            // first, a node is only ever created for a deliverable that is
            // already recorded, so this path can no longer leave a node in the
            // tree with no artifact behind it at all.
            if let Some(node_id) = prior_node.as_deref() {
                // Inherit before storing, so a failure anywhere below leaves
                // the version pointing at the node that currently holds it.
                record.stamp_workspace_node(node_id);
            }
            artifacts.upsert(self.company, &record).await?;

            // **A failed mirror does not lose the deliverable.** An explicit
            // publish that could not be filed into the tree is still recorded
            // as an artifact — dropping a produced file over tree bookkeeping
            // would be far worse than a deliverable the operator has to reach
            // through the Artifacts tab. So this logs at `error` (loudly: the
            // tree is where people look) and leaves the version unlinked, which
            // is exactly what a pre-#552 record carries. The next publish of
            // the same source retries and heals it.
            if let Some(workspace) = self.deps.workspace.as_ref() {
                let target = artifact_mirror::PublishTarget {
                    agent_id: author,
                    task_id: &card.id,
                    // Issue #1687: the folder the deliverable lands in is
                    // named for the work, not only keyed by it. The card is
                    // right here and its title is the one string that says
                    // what an operator is looking at.
                    task_title: Some(card.title.as_str()),
                    source: &pending.source,
                    payload: match &pending.payload {
                        crate::harness::publish::PublishPayload::Text(text) => {
                            artifact_mirror::MirrorPayload::Text(text)
                        }
                        crate::harness::publish::PublishPayload::Bytes { bytes, mime } => {
                            artifact_mirror::MirrorPayload::Bytes { bytes, mime }
                        }
                    },
                    existing_node_id: prior_node.as_deref(),
                };
                match artifact_mirror::materialize(workspace.as_ref(), self.company, target).await {
                    Ok(mirrored) => {
                        let node_id = mirrored.node_id;
                        // Issue #663/#668: the version body was composed before
                        // the store was asked, so it describes an outcome that
                        // had not happened. Now it has — say what it was, and
                        // record the digest the STORE computed so two versions
                        // of one binary can be told apart.
                        //
                        // Always re-composed, never conditional on the link
                        // having changed: an ordinary re-publish reuses its node
                        // and would otherwise keep the previous version's
                        // digest, which is precisely the "identical string"
                        // failure #668 describes.
                        let stored = pending.payload.artifact_body_for(
                            crate::harness::publish::PayloadStorage::Stored {
                                sha256: mirrored.sha256.as_deref(),
                            },
                        );
                        // Only when it actually says something new. Prose is its
                        // own body, so a text re-publish composes the identical
                        // string and still stores once — the contract
                        // `an_ordinary_republish_writes_the_artifact_once`
                        // pins. A binary's body gains the store's digest, so it
                        // differs and is worth the second write: without it the
                        // version would keep the PREVIOUS digest, which is the
                        // indistinguishable-versions defect (#668) with an extra
                        // step.
                        let body_changed =
                            record.latest().is_some_and(|latest| latest.body != stored);
                        if body_changed {
                            record.amend_latest_body(stored);
                        }
                        let relinked = record.workspace_node_id() != Some(node_id.as_str());
                        if relinked {
                            record.stamp_workspace_node(&node_id);
                        }
                        // A second write only when the record actually changed:
                        // a fresh publish, a re-publish whose node the operator
                        // deleted, or a body that now carries an outcome it did
                        // not before. Warn rather than `?` for the unchanged
                        // reason — BOTH surfaces already hold this body and only
                        // the record's copy is stale, so failing the batch would
                        // discard the remaining publishes' records to report
                        // something the next publish repairs.
                        if (body_changed || relinked)
                            && let Err(err) = artifacts.upsert(self.company, &record).await
                        {
                            tracing::warn!(
                                task_id = %card.id,
                                source = %pending.source,
                                node = %node_id,
                                error = %err,
                                "[publish] the deliverable and its note are both stored but the \
                                 record could not be updated; the next publish of this source \
                                 re-adopts the note and repairs it"
                            );
                        }
                    }
                    Err(err) => {
                        // Issue #663. The record already claimed this file was
                        // filed into the workspace. It was not, so the claim is
                        // withdrawn rather than left standing — an operator who
                        // opens the artifact and reads "open it there" and finds
                        // nothing is the dangling-record failure #553 set out to
                        // remove, arriving through the error path.
                        //
                        // The store's error is logged and NOT written to the
                        // record: a version body is permanent and a backend
                        // error can name host paths.
                        tracing::error!(
                            task_id = %card.id,
                            agent = %author,
                            source = %pending.source,
                            error = %err,
                            "[publish] could not put the published file into the company \
                             workspace; the artifact record says so rather than promising a \
                             file that is not there"
                        );
                        record.amend_latest_body(
                            pending.payload.artifact_body_for(
                                crate::harness::publish::PayloadStorage::Refused,
                            ),
                        );
                        if let Err(err) = artifacts.upsert(self.company, &record).await {
                            tracing::error!(
                                task_id = %card.id,
                                source = %pending.source,
                                error = %err,
                                "[publish] the workspace refused the file AND the record could \
                                 not be corrected; it still claims the file is stored"
                            );
                        }
                    }
                }
            }
            written.push(TaskOutputArtifact {
                artifact_id: record.id.clone(),
                version,
                title: record.title.clone(),
                kind: record.kind,
            });
            self.deps.pending_publishes.output_collector().artifact(
                record.id.clone(),
                card.id.clone(),
                version,
                &record.title,
            );
            // Keep the working set current so two publishes of the same path in
            // one run extend one record rather than opening two.
            on_card.push(record);
        }
        Ok(written)
    }

    /// Records what a **conversation** turn published, minting the card that
    /// carries it (issue #445). Returns that card's id.
    ///
    /// # Why a card, rather than a company-level artifact
    ///
    /// The issue allows either: a chat deliverable becomes an artifact attached
    /// to no card, or the act of publishing mints the card. This path takes the
    /// second, and the deciding argument is *reachability* — which is, after
    /// all, the entire bug.
    ///
    /// An [`ArtifactRecord`] carries a non-optional `task_id`, `(task_id,
    /// source)` **is** its identity, the only route that lists artifacts is
    /// `GET /tasks/{task_id}/artifacts`, and the only console surface that
    /// renders one is the per-task Artifacts tab. A card-less artifact would
    /// therefore need an optional `task_id` (breaking the identity contract), a
    /// new company-scoped route, and a new console view — and until that last
    /// piece shipped, the artifact would be recorded and still unreachable,
    /// which is precisely the failure being fixed, merely moved one layer down.
    /// Minting the card reuses a path the operator can already open today.
    ///
    /// It is also honest about what happened rather than a workaround: an agent
    /// that produced a deliverable did a unit of work, and a board that shows it
    /// is more accurate than one that does not. The card lands in
    /// [`COLUMN_IN_REVIEW`] because that is where the lifecycle already puts
    /// finished agent work awaiting a person — `COLUMN_DONE` is reached only by
    /// a human accepting it, and this fix does not get to decide that on their
    /// behalf.
    ///
    /// # What it deliberately does not do
    ///
    /// No `output` stamp. That field pins a `run_id` and an attempt ordinal, and
    /// a chat turn has neither — inventing one would put a fabricated attempt on
    /// a card to make a field look populated. The artifacts are reachable
    /// through the tab regardless; an invented run id would not be true.
    pub(crate) async fn record_conversation_publishes(
        &self,
        responder: &str,
        chat: ChatTarget<'_>,
        published: Vec<publish::PendingPublish>,
    ) -> crate::Result<String> {
        if published.is_empty() {
            // Every known caller filters this out before reaching here; this
            // stays unreachable the same way the check below does, so a
            // future caller cannot mint a card for a deliverable that is not
            // there.
            return Err(crate::OpenCompanyError::Harness(
                "a conversation minted a card with nothing published".to_string(),
            ));
        }
        let Some(tasks) = self.deps.tasks.as_ref() else {
            // Unreachable while the claim is only taken with both stores wired,
            // and an error rather than a silent `Ok` so it stays unreachable:
            // the caller surfaces this to the operator instead of dropping the
            // deliverable the way #445 did.
            return Err(crate::OpenCompanyError::Harness(
                "a conversation published a file but no task board is wired".to_string(),
            ));
        };

        let card = TaskRecord {
            id: generate_id(),
            title: crate::ports::tasks::TaskTitle::system(&publish::conversation_card_title(
                &published,
            )),
            note: Some(publish::conversation_card_note(responder, &published)),
            // Finished agent work a person has not accepted yet — the same
            // landing `column_for_settled_run(Succeeded)` gives a dispatched run.
            column: COLUMN_IN_REVIEW.to_string(),
            priority: "medium".to_string(),
            assignee: responder.to_string(),
            updated_at_millis: now_millis(),
            // The conversation this came out of, so the card points back at the
            // thread that produced it (#151 §3.2's field, same meaning).
            // Issue #1890 B: and the thread inside it, so a file published
            // inside a thread leaves its card pointing at that thread rather
            // than at the channel around it. `None` for the thread is the
            // channel-level conversation, which is where every publish landed
            // before threads were part of the key.
            origin: TaskOrigin::new(chat.chat_id.map(str::to_string), chat.thread_root),
            // A chat turn has no card in scope, so this is a lineage root —
            // the same `None` a `spawn_task` from an ordinary chat turn writes.
            parent_task_id: None,
            output: None,
            plan: None,
            planning_attempts: Vec::new(),
            deliverable: crate::ports::tasks::TaskDeliverable::Once,
            workflow_proposal: None,
            origin_run_id: None,
            origin_workflow_id: None,
            origin_message_seq: None,
            bounced: None,
        };
        // The card is written **first**: an artifact's `task_id` must name a
        // card that exists. If the artifact writes then fail, the failure
        // direction is a visible card whose note explains what it was for —
        // recoverable, and the operator is told below. The reverse order would
        // leave artifacts pointing at a card that was never created, which is
        // unreachable by every route and indistinguishable from the original
        // bug.
        tasks.upsert(self.company, &card).await?;

        // No run id: there is no attempt row behind a chat turn, and
        // `stamp_run` is skipped rather than given something invented.
        let recorded = self
            .record_published_artifacts(&card, responder, published, None)
            .await?;
        tracing::info!(
            task_id = %card.id,
            agent = %responder,
            artifacts = recorded.len(),
            "[publish] a conversation published files; minted a card to carry them"
        );
        Ok(card.id)
    }
}
