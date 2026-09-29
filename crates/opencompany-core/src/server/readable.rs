//! The display projection for agent-written text.
//!
//! Agents are told to write for a person and keep ids in their tool calls,
//! and most of the time they do. This is the safety net at the display edge:
//! a roster or desk id that still reaches a reply is shown as the name a
//! person knows, and the pooled turn's `[conversation: …]` preamble is
//! dropped if a model echoed it back.
//!
//! Display only. The journal keeps exactly what the model wrote, and every
//! agent-facing reader goes on reading that.

use std::collections::HashMap;
use std::ops::Range;

use crate::ports::types::{CompanyRecord, Mention};
use crate::server::chat_history::{MentionView, MessageView, ReferralLine};

/// Teammate and desk ids mapped to the names a person reads.
///
/// Built once per request from the live company record.
#[derive(Debug, Clone, Default)]
pub struct DisplayNames {
    by_id: HashMap<String, String>,
}

impl DisplayNames {
    /// Names for every teammate and desk on `record`.
    ///
    /// A teammate is named by their display name, or their role when they
    /// have none. An id whose display text is itself another known id is left
    /// out, so projecting twice never rewrites a name a second time.
    #[must_use]
    pub fn from_record(record: &CompanyRecord) -> Self {
        let manifest_roster = record.effective_agents();
        let mut pairs: Vec<(String, String)> = manifest_roster
            .iter()
            .map(|agent| {
                let label = agent
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(agent.role.trim());
                (agent.id.clone(), label.to_string())
            })
            .collect();
        pairs.extend(
            record
                .overlay_agents
                .iter()
                .filter(|overlay| !record.is_retired(&overlay.id))
                .filter(|overlay| !manifest_roster.iter().any(|agent| agent.id == overlay.id))
                .map(|overlay| {
                    let label = Some(overlay.name.trim())
                        .filter(|name| !name.is_empty())
                        .unwrap_or(overlay.role.trim());
                    (overlay.id.clone(), label.to_string())
                }),
        );
        pairs.extend(
            record
                .manifest
                .group_chats
                .iter()
                .map(|desk| (desk.id.clone(), desk.name.trim().to_string())),
        );
        pairs.extend(
            record
                .overlay_desks
                .iter()
                .map(|desk| (desk.id.clone(), desk.name.trim().to_string())),
        );
        Self::from_pairs(pairs)
    }

    /// Names for `runtime`'s company, or none when its record cannot be read.
    pub async fn load(runtime: &crate::company::runtime::CompanyRuntime) -> Self {
        match runtime.store().load(runtime.id()).await {
            Ok(Some(record)) => Self::from_record(&record),
            Ok(None) => Self::default(),
            Err(error) => {
                tracing::debug!(
                    company = %runtime.id(),
                    %error,
                    "[chat] display names unavailable; replies render as written"
                );
                Self::default()
            }
        }
    }

    /// Names for `runtime`'s company, or `None` when the read itself failed.
    ///
    /// Unlike [`Self::load`], a store error yields `None` rather than an
    /// empty map, so a caller refreshing a cached value can keep the last
    /// good one instead of clearing it on a transient read failure.
    pub async fn try_load(runtime: &crate::company::runtime::CompanyRuntime) -> Option<Self> {
        match runtime.store().load(runtime.id()).await {
            Ok(Some(record)) => Some(Self::from_record(&record)),
            Ok(None) => Some(Self::default()),
            Err(error) => {
                tracing::debug!(
                    company = %runtime.id(),
                    %error,
                    "[chat] display names refresh failed; keeping the cached names"
                );
                None
            }
        }
    }

    fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut by_id: HashMap<String, String> = HashMap::new();
        for (id, label) in pairs {
            if !label.is_empty() && label != id {
                by_id.entry(id).or_insert(label);
            }
        }
        let ids: Vec<String> = by_id.keys().cloned().collect();
        by_id.retain(|_, label| !ids.contains(label));
        Self { by_id }
    }

    /// The name a person knows `id` by, when it is a known teammate or desk.
    #[must_use]
    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.by_id.get(id).map(String::as_str)
    }
}

/// Agent-written `text` as a person reads it. See the module docs.
pub(crate) fn readable_moves(text: String, names: &DisplayNames) -> String {
    project(&text, names, &[]).text
}

/// Projects every agent-written body in a page of history, in place.
///
/// A person's own message is left exactly as they typed it.
pub(crate) fn project_history(messages: &mut [MessageView], names: &DisplayNames) {
    for view in messages {
        if !view.by_person {
            let spans: Vec<Range<usize>> = view.mentions.iter().map(mention_span).collect();
            let projected = project(&view.text, names, &spans);
            for mention in &mut view.mentions {
                mention.offset = projected.offset(mention.offset);
            }
            view.text = projected.text;
        }
        if let Some(conversation) = view.referral_conversation.as_mut() {
            project_lines(&mut conversation.lines, names);
        }
        for conversation in &mut view.agent_conversations {
            project_lines(&mut conversation.lines, names);
        }
    }
}

/// An agent reply's body projected for a person, with its mentions' spans
/// left exactly as written so their offsets can follow via
/// [`Projected::offset`].
pub(crate) fn project_reply(text: &str, mentions: &[Mention], names: &DisplayNames) -> Projected {
    let spans: Vec<Range<usize>> = mentions
        .iter()
        .map(|mention| mention.offset..mention.offset + mention.text.len())
        .collect();
    project(text, names, &spans)
}

fn project_lines(lines: &mut [ReferralLine], names: &DisplayNames) {
    for line in lines {
        line.text = readable_moves(std::mem::take(&mut line.text), names);
        if let Some(name) = names.name_of(&line.author_label) {
            line.author_label = name.to_string();
        }
    }
}

fn mention_span(mention: &MentionView) -> Range<usize> {
    mention.offset..mention.offset + mention.text.len()
}

/// Projected text, and where each original byte offset landed in it.
#[derive(Debug)]
pub(crate) struct Projected {
    /// The text as a person reads it.
    pub(crate) text: String,
    edits: Vec<Edit>,
}

#[derive(Debug)]
struct Edit {
    at: usize,
    removed: usize,
    inserted: usize,
}

impl Projected {
    /// Where byte `offset` of the original text is in [`Self::text`].
    ///
    /// Meant for offsets outside every rewritten span, which is what a
    /// protected span guarantees.
    pub(crate) fn offset(&self, offset: usize) -> usize {
        let mut delta: isize = 0;
        for edit in &self.edits {
            if edit.at + edit.removed > offset {
                break;
            }
            delta += edit.inserted as isize - edit.removed as isize;
        }
        offset.saturating_add_signed(delta)
    }
}

const CONVERSATION_PREFIX: &str = "[conversation: ";

/// Rewrites known ids in `text` to names, leaving `protected` byte ranges,
/// fenced code and unknown ids untouched.
///
/// An id is rewritten only where it is unambiguous: in backticks on its own,
/// after an `@`, or bare when it contains `_` or `-` and stands as a whole
/// word outside a path, address or file name.
pub(crate) fn project(text: &str, names: &DisplayNames, protected: &[Range<usize>]) -> Projected {
    let mut out = String::with_capacity(text.len());
    let mut edits = Vec::new();
    let mut pos = 0;
    let mut stripped = false;
    while text[pos..].starts_with(CONVERSATION_PREFIX) {
        let line_end = text[pos..].find('\n').map_or(text.len(), |at| pos + at);
        if !text[pos..line_end].trim_end().ends_with(']') {
            break;
        }
        let next = (line_end + 1).min(text.len());
        edits.push(Edit {
            at: pos,
            removed: next - pos,
            inserted: 0,
        });
        pos = next;
        stripped = true;
    }
    let prefix_edits = edits.len();
    // The fence character and opener length while inside a fenced block, so a
    // closing fence has to match CommonMark rules: same character as the
    // opener, at least as long, with nothing but whitespace after it. A
    // shorter or differently-charactered line (e.g. a nested ``` inside a
    // ```` block) then stays inside the fence instead of closing it early.
    let mut open_fence: Option<(u8, usize)> = None;
    for line in text[pos..].split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed
            .bytes()
            .next()
            .filter(|byte| *byte == b'`' || *byte == b'~');
        let run = marker.map_or(0, |byte| trimmed.bytes().take_while(|b| *b == byte).count());
        let fence = match (open_fence, marker) {
            (None, Some(byte)) if run >= 3 => {
                open_fence = Some((byte, run));
                true
            }
            (Some((byte, len)), Some(closing))
                if closing == byte && run >= len && trimmed[run..].trim().is_empty() =>
            {
                open_fence = None;
                true
            }
            _ => false,
        };
        if fence || open_fence.is_some() || names.by_id.is_empty() {
            out.push_str(line);
        } else {
            rewrite_line(line, pos, names, protected, &mut out, &mut edits);
        }
        pos += line.len();
    }
    let rewrites = edits.len() - prefix_edits;
    if rewrites > 0 || stripped {
        tracing::debug!(
            rewrites,
            stripped_prefix = stripped,
            "[chat] display projection rewrote ids in an agent reply"
        );
    }
    Projected { text: out, edits }
}

fn is_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn id_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|byte| !is_id_byte(*byte))
        .map_or(bytes.len(), |at| from + at)
}

fn ends_as_word(bytes: &[u8], end: usize) -> bool {
    match bytes.get(end) {
        None => true,
        Some(b'/' | b'@') => false,
        Some(b'.') => !bytes.get(end + 1).is_some_and(|next| is_id_byte(*next)),
        Some(byte) => !is_id_byte(*byte),
    }
}

fn rewrite_line(
    line: &str,
    base: usize,
    names: &DisplayNames,
    protected: &[Range<usize>],
    out: &mut String,
    edits: &mut Vec<Edit>,
) {
    let bytes = line.as_bytes();
    let mut replace = |out: &mut String, at: usize, removed: usize, name: &str| {
        out.push_str(name);
        edits.push(Edit {
            at: base + at,
            removed,
            inserted: name.len(),
        });
    };
    let mut i = 0;
    while i < bytes.len() {
        if let Some(span) = protected.iter().find(|span| span.contains(&(base + i))) {
            let end = (span.end - base).min(bytes.len());
            out.push_str(&line[i..end]);
            i = end;
            continue;
        }
        let prev = i.checked_sub(1).map(|at| bytes[at]);
        match bytes[i] {
            b'`' => {
                let Some(close) = line[i + 1..].find('`').map(|at| i + 1 + at) else {
                    out.push('`');
                    i += 1;
                    continue;
                };
                match names.name_of(&line[i + 1..close]) {
                    Some(name) => replace(out, i, close + 1 - i, name),
                    None => out.push_str(&line[i..=close]),
                }
                i = close + 1;
            }
            b'@' if !prev.is_some_and(is_id_byte) => {
                let end = id_end(bytes, i + 1);
                match names.name_of(&line[i + 1..end]) {
                    Some(name) if end > i + 1 && ends_as_word(bytes, end) => {
                        replace(out, i, end - i, name);
                    }
                    _ => out.push_str(&line[i..end]),
                }
                i = end.max(i + 1);
            }
            byte if is_id_byte(byte) => {
                let end = id_end(bytes, i);
                let token = &line[i..end];
                let bare = !matches!(prev, Some(b'/' | b'.' | b'@' | b':'))
                    && (token.contains('_') || token.contains('-'))
                    && ends_as_word(bytes, end);
                match names.name_of(token) {
                    Some(name) if bare => replace(out, i, end - i, name),
                    _ => out.push_str(token),
                }
                i = end;
            }
            _ => {
                let width = line[i..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&line[i..i + width]);
                i += width;
            }
        }
    }
}

#[cfg(test)]
#[path = "readable_tests.rs"]
mod tests;
