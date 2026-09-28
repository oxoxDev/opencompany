//! The mention seam: the ports resolution and notification need, bundled into
//! one value any journaling surface can hold.
//!
//! [`CompanyRuntime`](crate::company::runtime::CompanyRuntime) delegates its
//! mention methods here and the hive's episode host holds a clone, so an
//! operator message and a desk reply obey one rule about who `@ada` is and
//! file one shape of notification.

use std::sync::Arc;

use crate::ports::types::{Actor, ActorKind, CompanyId, EventSeq, Mention};
use crate::ports::{CompanyStore, notifications::NotificationStore, users::UserStore};

/// The company store, user directory and notification store the mention
/// pipeline reads and writes.
#[derive(Clone)]
pub struct MentionSeam {
    store: Arc<dyn CompanyStore>,
    users: Arc<dyn UserStore>,
    notifications: Arc<dyn NotificationStore>,
}

impl MentionSeam {
    /// Builds a seam over the three ports.
    #[must_use]
    pub fn new(
        store: Arc<dyn CompanyStore>,
        users: Arc<dyn UserStore>,
        notifications: Arc<dyn NotificationStore>,
    ) -> Self {
        Self {
            store,
            users,
            notifications,
        }
    }

    /// Resolve the mentions in one chat message body.
    ///
    /// The single seam both journal sites go through, so an operator message
    /// and an agent reply cannot end up obeying different rules about who
    /// `@ada` is. Loads the record and the user directory and hands them to
    /// [`crate::runtime::mentions::resolve`], which does the rest without
    /// touching IO.
    ///
    /// **Never fails a send.** A store that cannot answer means mentions cannot
    /// be resolved, not that the message cannot be delivered — so a read error
    /// yields an empty list and is logged. The message still lands; it simply
    /// draws no chips and pings nobody, which is the same state every message
    /// journaled before this feature existed is in.
    pub async fn resolve_mentions(
        &self,
        id: &CompanyId,
        text: &str,
        supplied: Option<Vec<Mention>>,
        sender: Option<&Actor>,
    ) -> Vec<Mention> {
        self.resolve_mentions_reporting(id, text, supplied, sender)
            .await
            .mentions
    }

    /// [`resolve_mentions`](Self::resolve_mentions), also reporting every
    /// `@name` that matched more than one thing and therefore matched nobody
    /// (B-101).
    ///
    /// The refusal itself is correct and long-standing — see
    /// [`crate::runtime::mentions`], never guess a ping — but it used to be
    /// announced only by the *absence* of a chip. An absence is not a signal: it
    /// is invisible in a wall of text and completely invisible over the API, so
    /// a founder's `@Priya` reached neither the teammate nor the person of that
    /// name, the channel's catch-all answered, and the reply talked about her in
    /// the third person. Whoever refuses has to be the one who says so, which is
    /// why this is here and not a second guess in the console.
    pub async fn resolve_mentions_reporting(
        &self,
        id: &CompanyId,
        text: &str,
        supplied: Option<Vec<Mention>>,
        sender: Option<&Actor>,
    ) -> crate::runtime::mentions::Extraction {
        // Issue: on the operator-message path this runs BEFORE the journal
        // append (`mention_responder` reads the resolved mentions off the
        // journaled event, so the append cannot go first), which puts these
        // two store reads in front of every chat POST's accept latency. Run
        // together rather than sequentially — they read different stores and
        // neither depends on the other's result — to keep that addition close
        // to the cost of the slower read alone rather than the sum of both.
        let (record, user_list) = tokio::join!(self.store.load(id), self.users.list_users(id));
        let record = match record {
            Ok(Some(record)) => record,
            Ok(None) => return Default::default(),
            Err(err) => {
                tracing::warn!(
                    company = %id,
                    error = %err,
                    "[mentions] the company record could not be read; this message is \
                     journaled with no mentions"
                );
                return Default::default();
            }
        };
        let mut users = user_list.unwrap_or_else(|err| {
            tracing::warn!(
                company = %id,
                error = %err,
                "[mentions] the user directory could not be read; only teammates and \
                 desks are resolvable on this message"
            );
            Vec::new()
        });
        // Suspended users are retained only for attribution and are refused on
        // every request — they must not be a live mention target here either.
        users.retain(|u| u.status == crate::ports::users::UserStatus::Active);
        // Sorted by the same stable key `GET .../chat/mentionables` uses before
        // it mints slugs (`user_slugs`), so a collision between two same-named
        // users gets the same `-2`/`-3` suffix here that the picker advertised —
        // an unsorted `UserStore` order (most-recently-created first) could
        // otherwise resolve `@sam-2` to a different person than the one the
        // picker showed under that label.
        users.sort_by(|a, b| a.id.cmp(&b.id));
        crate::runtime::mentions::resolve_reporting(text, supplied, sender, &record, &users)
    }

    /// The console channel id a mention in `desk` belongs to.
    ///
    /// A desk channel's id is its own thread id, so the context is the desk id
    /// unchanged. A DM's thread id is the bare roster teammate id, while the
    /// console's channel id for the same DM is `dm:<teammate-id>` — and the
    /// console addresses a DM with that bare id (ChatView sends
    /// `active.member.id`). So a mention in a DM has to be re-keyed into the
    /// console's channel-id space or the rail has no row to badge, and opening
    /// the DM can never match or clear the notification.
    ///
    /// The roster check goes through [`crate::runtime::assignee::resolve`] for
    /// its desk-first ordering: the same one `responder_for` uses, so a desk
    /// whose id happens to match a teammate id still stores the desk id, and a
    /// desk literally named `dm:<…>` keeps that id instead of being displaced
    /// by the `dm:`-stripped retry. The human user directory is deliberately
    /// consulted **only** when the store will not answer — never ahead of that
    /// resolution, or a desk id matching a human id would be misclassified as
    /// `dm:<id>`. The resolution carries the **canonical** id (issue #214), so
    /// a key typed as a display name — `chat: "Engineering"` for a desk whose
    /// id is `engineering` — stores the canonical id, which is what the rail's
    /// channel ids are built from. A `dm:`-prefixed key is tried **as sent**
    /// first and only split for the retry when it names nothing — so a
    /// noncanonical address — `dm:BACKEND_ENGINEER`, `dm:<display name>` —
    /// still stores `dm:<canonical-agent-id>` and badges the rail's real DM
    /// channel rather than one that does not exist.
    pub(crate) async fn mention_context(
        &self,
        id: &CompanyId,
        users: &[crate::ports::users::UserRecord],
        desk: &str,
    ) -> String {
        // The key is tried **as sent** first, exactly as the routing does: a
        // desk or teammate literally named `dm:x` resolves today, and an
        // unconditional prefix-strip would let `dm:x` claim it
        // ([`crate::runtime::assignee::dm_key`] documents that ordering). The
        // stripped retry below is only for a `dm:`-prefixed key that names
        // nothing as sent.
        let Ok(Some(record)) = self.store.load(id).await else {
            // Store will not answer; best-effort, same as the callers. A
            // canonical `dm:<teammate-id>` still badges through the raw key,
            // and a *noncanonical* roster key is re-keyed through the
            // directory. This runs only on the store-down path, never ahead of
            // `assignee::resolve`: a desk id that happens to match a human id
            // must still file under the desk when the store answers, or a
            // mention aimed at that desk would badge a nonexistent `dm:<id>`
            // channel.
            if users.iter().any(|u| u.id == desk) {
                return format!("dm:{desk}");
            }
            if let Some(bare) = crate::runtime::assignee::dm_key(desk)
                && users.iter().any(|u| u.id == bare)
            {
                return format!("dm:{bare}");
            }
            return desk.to_string();
        };
        let bare = crate::runtime::assignee::dm_key(desk);
        match crate::runtime::assignee::resolve(&record, desk) {
            // A bare teammate key files under the console's DM channel id,
            // canonicalized (issue #214) — as does a teammate literally named
            // `dm:<…>`, whose DM channel id is `dm:dm:<…>` in the same space.
            crate::runtime::assignee::AssigneeResolution::Agent(agent) => format!("dm:{agent}"),
            // A desk with no member to work it is still a real desk with a real
            // rail channel, so it files under the same canonical id as one with
            // a lead — a memberless `"Sales"` still has to badge `#sales`.
            crate::runtime::assignee::AssigneeResolution::Desk { desk: desk_id, .. }
            | crate::runtime::assignee::AssigneeResolution::EmptyDesk(desk_id) => desk_id,
            // Unassigned, unknown, or ambiguous. A `dm:`-prefixed key that
            // names nothing as sent can still be the console's DM channel for a
            // *noncanonical* address — `dm:BACKEND_ENGINEER`,
            // `dm:<display name>` — which the routing resolves
            // case-insensitively, so the stored context has to carry the
            // canonical agent id the rail's channel ids are keyed by. Storing
            // the raw key files the badge under a channel that does not exist,
            // and opening the actual DM can never clear it. Split the prefix
            // off and run the bare half through the same resolution as an
            // un-prefixed desk, re-applying the prefix only when it names a
            // teammate.
            _ => {
                if let Some(bare) = bare {
                    match crate::runtime::assignee::resolve(&record, bare) {
                        crate::runtime::assignee::AssigneeResolution::Agent(agent) => {
                            return format!("dm:{agent}");
                        }
                        crate::runtime::assignee::AssigneeResolution::Desk {
                            desk: desk_id,
                            ..
                        }
                        | crate::runtime::assignee::AssigneeResolution::EmptyDesk(desk_id) => {
                            return desk_id;
                        }
                        _ => {}
                    }
                }
                let probe = bare.unwrap_or(desk);
                if crate::ports::general_channel::is_general_spelling(probe) {
                    crate::ports::general_channel::GENERAL_CHANNEL_ID.to_string()
                } else {
                    desk.to_string()
                }
            }
        }
    }

    /// Files a durable mention notification for the people `mentions` names in
    /// `desk` (the console's channel-id space), for the journaled message at
    /// `message_seq`.
    ///
    /// **One row, many recipients** — not one row each. Read state is already
    /// per `(company, user, notification)`, so a single row carrying an
    /// audience gives every recipient independent read state for free, and the
    /// feed does not grow by the size of the room every time somebody types
    /// `@everyone`. Teammates produce no notification: an agent has no inbox to
    /// badge and no person to interrupt; a mention of one is already handled by
    /// routing.
    ///
    /// Shared by the operator `/chat` path and the approval-continuation path,
    /// so an `@user` an agent types back badges and notifies whoever it names
    /// whichever journaling surface wrote the reply. Without this, a
    /// continuation's mentions rendered as chips and nothing else — the badge
    /// and the notification both silently missing for exactly the person they
    /// are meant to reach: offline when the reply lands.
    pub(crate) async fn notify_mentions(
        &self,
        id: &CompanyId,
        mentions: &[Mention],
        message_seq: &EventSeq,
        by: Option<&Actor>,
        desk: &str,
    ) {
        let users = match self.users.list_users(id).await {
            Ok(users) => users,
            Err(err) => {
                tracing::warn!(
                    company = %id,
                    error = %err,
                    "[mentions] the user directory could not be read; this message badges nobody"
                );
                return;
            }
        };
        let users: Vec<_> = users
            .into_iter()
            .filter(|u| u.status == crate::ports::users::UserStatus::Active)
            .collect();
        let mut audience = crate::runtime::mentions::mentioned_users(&users, mentions);
        // Never notify the author, even when they wrote `@everyone`. `normalize`
        // already drops a direct self-mention, but a broadcast expands to the
        // whole company *after* that, so this is the only place the author can
        // be removed from one.
        if let Some(Actor {
            kind: ActorKind::User,
            id: author,
        }) = by
        {
            audience.retain(|u| u != author);
        }
        if audience.is_empty() {
            return;
        }

        let who = by
            .filter(|a| a.kind == ActorKind::User)
            .and_then(|a| users.iter().find(|u| u.id == a.id))
            .map(crate::runtime::mentions::user_label)
            .unwrap_or_else(|| "Someone".to_string());
        let note = crate::ports::notifications::Notification {
            id: crate::ports::generate_id(),
            kind: "mention".to_string(),
            subject: crate::ports::notifications::Subject {
                kind: crate::ports::notifications::SubjectKind::Message,
                id: message_seq.value().to_string(),
            },
            created_at: crate::ports::now_millis(),
            title: format!("{who} mentioned you in {desk}"),
            audience: Some(audience),
            // The console's channel-id space, so a badge lands without the
            // browser having loaded that transcript. Whether the thread is a DM
            // is a question about the roster, not the human user directory —
            // see [`Self::mention_context`].
            context: Some(self.mention_context(id, &users, desk).await),
        };
        if let Err(err) = self.notifications.append(id, &note).await {
            tracing::warn!(
                company = %id,
                error = %err,
                "[mentions] a mention could not be recorded; the message still lands and \
                 still renders, but nobody is badged for it"
            );
        }
    }
}
