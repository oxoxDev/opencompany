//! What a ledger *is*, as data rather than as a Rust module.
//!
//! The board was a Rust type with six hard-coded columns
//! ([`BOARD_COLUMNS`](crate::ports::tasks::BOARD_COLUMNS)), and that is right
//! for the one axis whose columns fire dispatch. It is wrong for everything
//! else a company records. A company keeps decisions, risks, hiring pipelines,
//! customer promises, invoices chased, experiments run — and every one of those
//! is *a set of rows with an id, a status and some prose*, which is the same
//! shape the board already is. Without a way to declare one, an agent asked to
//! track them puts them in a note, a chat message, or a workspace file nobody
//! designed, and the company's own record of what it decided is prose scattered
//! across three surfaces.
//!
//! So a ledger is a [`LedgerSpec`]: a declaration the runtime ships a few of
//! and a workspace writes the rest of. The engine renders both the same way.
//!
//! # What a declaration deliberately cannot do
//!
//! - **It cannot reason.** [`Check`] is a closed set — a required field, an
//!   unknown status, a close with no reason. There is no expression language,
//!   and there should not be: a company that could write predicates into a
//!   ledger declaration has written a rules engine nobody can review.
//! - **It cannot raise a bound.** Every `cap` is clamped against
//!   [`super::budget`] on the way in, so a declaration cannot grow its own file
//!   past what a reader (or a turn) is asked to carry.
//! - **It cannot shadow a built-in, or claim a built-in's derived path.** Every
//!   prompt and every route naming `tasks` is written against what `tasks`
//!   holds, and two writers on one derived file is how each one's work
//!   disappears.

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::error::OpenCompanyError;

/// The workspace folder every derived ledger file is written into.
///
/// The folder name is the invariant: **nothing in it is hand-written**. The
/// workspace write guard refuses an edit to a path under it and names the tool
/// that actually writes the row, because an edit that lands and is then erased
/// by the next derivation is worse than one that is refused — the author
/// believes it took.
pub const DERIVED_DIR: &str = "derived";

/// Where a ledger's entries come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerSource {
    /// An append-only event log the engine folds into entries.
    ///
    /// Events fold into current state rather than replacing it, which is what
    /// makes *what was done and why* survivable. A board rewritten wholesale
    /// loses its finished rows on every rewrite and nothing says so; a fold
    /// keeps every one, because closing a row is an event like any other.
    ///
    /// The only source a workspace may declare.
    #[default]
    Events,
    /// A ledger some other part of the runtime already owns and renders — the
    /// task board, the run history.
    ///
    /// Registered anyway so `list_ledgers` names every ledger and `read_ledger`
    /// reads every ledger. A discovery surface that covers four of six is one
    /// an agent stops trusting, and then stops using.
    Native,
}

/// What a field is *for*, which is what the engine needs to know about it.
///
/// A bare name would leave the engine guessing which field is the identity and
/// which is prose to truncate. Guessing wrong is how a ledger renders its id
/// cut short and its essay whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldRole {
    /// The entry's identity. Exactly one per spec.
    Id,
    /// The one-line summary a rendered row and an index line carry.
    Title,
    /// Which of [`LedgerSpec::statuses`] the entry is in.
    Status,
    /// Long text: truncated in every rendered section, whole through a read.
    #[default]
    Prose,
    /// Ids, urls or paths this entry points at.
    Refs,
    /// A person or desk this entry belongs to.
    Owner,
    /// An ISO-8601 date (`2026-08-17`) or epoch-millis instant.
    Date,
    /// A number — an amount, a score, a count. Stored as text and never
    /// arithmetic'd: the ledger has no opinion about it, which is what lets a
    /// row carry the highest score and still be closed as rejected.
    Number,
}

/// One field of an entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// The key an event sets and a rendered row labels.
    pub name: String,
    /// What the engine does with it. See [`FieldRole`].
    #[serde(default)]
    pub role: FieldRole,
    /// A short line saying what belongs here, shown to whoever writes the row.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Whether an entry missing this field is a reported fault.
    #[serde(default)]
    pub required: bool,
}

/// One status an entry may be in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSpec {
    /// The wire word an event writes.
    pub name: String,
    /// What a person reads, when it differs from the wire word.
    ///
    /// Empty for the ledgers a company declares, whose statuses are already
    /// written to be read (`open`, `at_risk`, `kept`); the console humanises
    /// those. It exists for the ones that are not: the board's columns are
    /// `in_progress` and `in_review`, and a console deriving "In progress" from
    /// the id gets `todo` → "Todo" wrong on the very first column.
    ///
    /// Carrying it on the wire is what let the console delete its own copy of
    /// the label table — the copy whose comment admitted a Rust test could not
    /// see it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Whether this status ends the entry's life.
    ///
    /// A closed entry is kept and rendered, never deleted. A known dead end is
    /// a result, and the reason it closed is the only thing that stops the next
    /// person — or the next agent — paying for it again.
    #[serde(default)]
    pub closed: bool,
    /// Whether closing into this status demands a `reason`.
    ///
    /// Plain snake_case on purpose (issue #1266's near-miss): this type is
    /// also what a declared ledger round-trips through in every store
    /// backend (`store/fs_ops.rs`, `sqlite.rs`, `mongodb.rs` all persist
    /// `LedgerSpec` via this same `Serialize`/`Deserialize`), so a rename
    /// here would make Serialize write one key and Deserialize expect
    /// another — silently dropping `needs_reason` back to `false` on the
    /// very next reload. The console-facing camelCase key belongs on a
    /// wire-only DTO instead — see `server::ops::ledgers::LedgerStatusDto`.
    #[serde(default)]
    pub needs_reason: bool,
}

/// The order a section lists its rows in.
///
/// Two values, and it must stay two. A section that could declare a sort
/// *expression* would be configuration deciding what somebody works on next,
/// which is a question about the company rather than about layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    /// First-seen order: the order ids arrived in the log.
    ///
    /// The default, so a ledger that says nothing renders the way a reader
    /// expects a list to keep.
    #[default]
    Recorded,
    /// Most-recently-touched first, by the position of the entry's **last**
    /// event rather than its first.
    ///
    /// Last-event, deliberately. Recording against an existing id merges into
    /// it, so an old row that is still the most important is raised by
    /// recording against it — with the tool every writer already holds and no
    /// new authority. Newest-created would fix the arriving row and
    /// permanently bury that one, which is the same failure in the other
    /// direction.
    Recent,
}

impl Order {
    /// The wire word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Recent => "recent",
        }
    }
}

/// The order names a declaration and a read both accept.
///
/// One list, so a bad `order` and a bad `sort` cannot drift into two different
/// messages about the same set of names.
pub const ORDERS: [&str; 2] = ["recorded", "recent"];

/// Reads an order name, or says which ones exist.
///
/// Never falls back silently: a mistyped order would otherwise render a section
/// in an order nobody asked for, with nothing saying so.
///
/// # Errors
///
/// Returns [`OpenCompanyError::InvalidRequest`] when `name` is not in
/// [`ORDERS`].
pub fn parse_order(name: &str) -> Result<Order> {
    match name.trim() {
        "recorded" => Ok(Order::Recorded),
        "recent" => Ok(Order::Recent),
        other => Err(invalid(format!(
            "`{other}` is not an order; use `recorded` or `recent`"
        ))),
    }
}

/// One rendered section of the derived file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// The heading a reader scans for.
    pub heading: String,
    /// The sentence under the heading saying what the section is for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blurb: String,
    /// Statuses whose entries appear here. Empty means every status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<String>,
    /// Rows this section renders. Clamped against [`super::budget::MAX_LISTED`]
    /// on the way in — never trusted from the declaration.
    #[serde(default = "default_cap")]
    pub cap: usize,
    /// The order those rows appear in. See [`Order`].
    #[serde(default)]
    pub order: Order,
}

fn default_cap() -> usize {
    super::budget::MAX_LISTED
}

/// A fault the engine reports, from a closed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Check {
    /// An entry missing a field the spec marks required.
    RequiredField,
    /// An entry whose status is not one the spec declares.
    KnownStatus,
    /// An entry closed into a `needs_reason` status with no reason given.
    ClosedNeedsReason,
}

/// The field a closing status is expected to fill in.
///
/// One name across every ledger rather than a per-spec choice: the check has to
/// know it, and a spec that could rename it would make the check unwritable.
/// *Why did this close* is the same question everywhere.
pub const REASON_FIELD: &str = "reason";

/// The longest a single field value may be, in codepoints.
///
/// A ledger row is a row, not a document. Text past this is truncated on the
/// way in rather than rejected, so a long paste still records — losing the tail
/// of an over-long value is a smaller failure than losing the whole write.
pub const MAX_FIELD_CHARS: usize = 4_000;

/// Fields one entry may carry.
pub const MAX_FIELDS: usize = 24;

/// Statuses one ledger may declare.
pub const MAX_STATUSES: usize = 16;

/// Sections one ledger may render.
pub const MAX_SECTIONS: usize = 8;

/// A ledger, declared.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSpec {
    /// Lowercase letters, digits and hyphens. How every tool and route names
    /// it, and — as `<slug>.json` — how it is stored.
    pub slug: String,
    /// The rendered file's heading.
    pub title: String,
    /// What this axis holds that no other ledger does, in a sentence or two.
    /// Carried into every listing, because a catalogue of slugs alone puts the
    /// answer behind a call somebody has to think to make.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub purpose: String,
    /// Where its entries come from. A declared ledger is always
    /// [`LedgerSource::Events`].
    #[serde(default)]
    pub source: LedgerSource,
    /// The workspace file this renders into, under [`DERIVED_DIR`].
    ///
    /// Optional on the wire, and deliberately: a caller declaring a ledger
    /// should not have to know the folder convention to succeed, and the
    /// commonest declaration a model writes omits it entirely. An absent value
    /// becomes `derived/<SLUG>.md`, which is what the caller meant. Naming one
    /// explicitly still works and is still held to the folder rule.
    #[serde(default)]
    pub derived: String,
    /// The fields an entry carries. Exactly one must have role
    /// [`FieldRole::Id`].
    pub fields: Vec<Field>,
    /// The statuses an entry may be in, with the ones that end its life marked
    /// `closed`.
    pub statuses: Vec<StatusSpec>,
    /// How the rendered file is laid out. Empty renders one section holding
    /// everything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    /// Faults to report on read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// Teammates or desks allowed to write it. Empty means anyone holding the
    /// tool.
    ///
    /// Checked at call time rather than at tool-registration time, because the
    /// set of ledgers is not fixed when tools are wired — a company may declare
    /// one afterwards. Holding `record_entry` is therefore not permission to
    /// write every ledger.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writers: Vec<String>,
    /// How this ledger is actually written, in a sentence.
    ///
    /// The write guard refuses an edit to a derived file and has to say what to
    /// do instead — and *what to do instead is not the same for every ledger*.
    /// An events ledger takes `record_entry`; the task board does not, and
    /// telling its caller otherwise sends them to a tool that refuses them a
    /// second time. So the answer travels with the declaration rather than
    /// being assumed at the point of refusal.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub written_by: String,
    /// Whether the runtime ships this one.
    ///
    /// A built-in may not be retired and its slug may not be shadowed, so
    /// `tasks` is always the task board and every prompt naming it stays right.
    #[serde(default)]
    pub builtin: bool,
}

impl LedgerSpec {
    /// The field carrying the entry's identity, if the spec declares one.
    pub fn id_field(&self) -> Option<&Field> {
        self.fields.iter().find(|field| field.role == FieldRole::Id)
    }

    /// The field carrying the one-line summary, if the spec declares one.
    pub fn title_field(&self) -> Option<&Field> {
        self.fields
            .iter()
            .find(|field| field.role == FieldRole::Title)
    }

    /// The field carrying the status, if the spec declares one.
    pub fn status_field(&self) -> Option<&Field> {
        self.fields
            .iter()
            .find(|field| field.role == FieldRole::Status)
    }

    /// Whether `status` is one this spec declares.
    pub fn knows_status(&self, status: &str) -> bool {
        self.statuses
            .iter()
            .any(|declared| declared.name.eq_ignore_ascii_case(status.trim()))
    }

    /// The declaration for `status`, if there is one.
    pub fn status(&self, status: &str) -> Option<&StatusSpec> {
        self.statuses
            .iter()
            .find(|declared| declared.name.eq_ignore_ascii_case(status.trim()))
    }

    /// Whether an entry in `status` is closed.
    ///
    /// Read off the declaration rather than off a status *name*, so a ledger a
    /// company defines mid-flight gets the same treatment as a built-in and the
    /// ledgers that declare no closed status are untouched.
    pub fn is_closed(&self, status: &str) -> bool {
        self.status(status).is_some_and(|declared| declared.closed)
    }

    /// Whether `writer` may write to this ledger.
    pub fn writable_by(&self, writer: &str) -> bool {
        self.writers.is_empty()
            || self
                .writers
                .iter()
                .any(|declared| declared.eq_ignore_ascii_case(writer.trim()))
    }

    /// The order a read uses when the call names none: the first section's.
    ///
    /// That is the ledger's own answer to what a reader arriving at it wants
    /// first — for the task board, "Do next". A ledger declaring no sections
    /// gets [`Order::Recorded`].
    pub fn default_order(&self) -> Order {
        self.sections
            .first()
            .map_or(Order::default(), |section| section.order)
    }

    /// Every status that closes an entry, in declaration order.
    pub fn closing_statuses(&self) -> Vec<&str> {
        self.statuses
            .iter()
            .filter(|status| status.closed)
            .map(|status| status.name.as_str())
            .collect()
    }

    /// Validates and clamps a declaration, whoever wrote it.
    ///
    /// Called on every path a spec enters by — a workspace's own declaration, a
    /// built-in at registry build, a spec loaded back off the store — so a
    /// declaration that was legal when it was written cannot outlive a bound
    /// that tightened. Clamping rather than rejecting on `cap` is deliberate:
    /// a stored ledger must keep rendering after a bound moves.
    ///
    /// # Errors
    ///
    /// Returns [`OpenCompanyError::InvalidRequest`] when the slug is not a
    /// slug, when the derived path is not a plain file under [`DERIVED_DIR`],
    /// when no field carries an id, when a status list is empty or duplicated,
    /// or when a section filters on a status the spec does not declare — each
    /// of which produces a ledger that renders but says nothing, which is worse
    /// than one that refuses.
    pub fn normalize(&mut self) -> Result<()> {
        self.slug = normalize_slug(&self.slug)?;
        self.title = trimmed_or(&self.title, &self.slug);
        self.purpose = self.purpose.trim().to_string();
        self.written_by = self.written_by.trim().to_string();
        if self.written_by.is_empty() {
            self.written_by =
                "`record_entry` to add or amend a row, `close_entry` to close one".to_string();
        }
        self.derived = normalize_derived(&self.derived, &self.slug)?;

        if self.fields.len() > MAX_FIELDS {
            return Err(invalid(format!(
                "a ledger carries at most {MAX_FIELDS} fields; `{}` declares {}",
                self.slug,
                self.fields.len()
            )));
        }
        for field in &mut self.fields {
            field.name = field.name.trim().to_string();
            field.description = field.description.trim().to_string();
            if field.name.is_empty() {
                return Err(invalid("every field needs a `name`"));
            }
        }
        let ids = self
            .fields
            .iter()
            .filter(|field| field.role == FieldRole::Id)
            .count();
        if ids != 1 {
            return Err(invalid(
                "a ledger needs exactly one field with role `id`; without it nothing can name a \
                 row to update or close it",
            ));
        }
        if let Some(duplicate) = first_duplicate(self.fields.iter().map(|field| &field.name)) {
            return Err(invalid(format!(
                "`{duplicate}` is declared twice as a field; two fields of one name is one field \
                 silently overwriting the other"
            )));
        }

        if self.statuses.is_empty() {
            return Err(invalid("a ledger needs at least one status"));
        }
        if self.statuses.len() > MAX_STATUSES {
            return Err(invalid(format!(
                "a ledger declares at most {MAX_STATUSES} statuses; `{}` declares {}",
                self.slug,
                self.statuses.len()
            )));
        }
        for status in &mut self.statuses {
            status.name = status.name.trim().to_ascii_lowercase();
            status.label = status.label.trim().to_string();
            if status.name.is_empty() {
                return Err(invalid("every status needs a `name`"));
            }
        }
        if let Some(duplicate) = first_duplicate(self.statuses.iter().map(|status| &status.name)) {
            return Err(invalid(format!(
                "`{duplicate}` is declared twice as a status"
            )));
        }

        if self.sections.len() > MAX_SECTIONS {
            return Err(invalid(format!(
                "a ledger renders at most {MAX_SECTIONS} sections; `{}` declares {}",
                self.slug,
                self.sections.len()
            )));
        }
        for section in &mut self.sections {
            section.heading = section.heading.trim().to_string();
            section.blurb = section.blurb.trim().to_string();
            if section.heading.is_empty() {
                return Err(invalid("every section needs a `heading`"));
            }
            for name in &mut section.statuses {
                *name = name.trim().to_ascii_lowercase();
            }
            if let Some(unknown) = section
                .statuses
                .iter()
                .find(|name| !self.statuses.iter().any(|status| &&status.name == name))
            {
                return Err(invalid(format!(
                    "section `{}` filters on status `{unknown}`, which `{}` does not declare — the \
                     section would always be empty",
                    section.heading, self.slug
                )));
            }
            // Clamped, not trusted. A declaration that could raise its own
            // bound is the one route back to the file the bounds exist to
            // prevent.
            section.cap = section.cap.clamp(1, super::budget::MAX_LISTED);
        }

        self.checks.sort_by_key(|check| format!("{check:?}"));
        self.checks.dedup();
        self.writers = self
            .writers
            .iter()
            .map(|writer| writer.trim().to_string())
            .filter(|writer| !writer.is_empty())
            .collect();
        Ok(())
    }
}

/// Reads a declaration from JSON, validating and clamping it.
///
/// # Errors
///
/// Returns [`OpenCompanyError::InvalidRequest`] when the document is not a
/// ledger declaration, or when [`LedgerSpec::normalize`] refuses it.
pub fn parse(document: &serde_json::Value, builtin: bool) -> Result<LedgerSpec> {
    let mut spec: LedgerSpec = serde_json::from_value(document.clone())
        .map_err(|error| invalid(format!("that is not a ledger declaration: {error}")))?;
    spec.builtin = builtin;
    spec.normalize()?;
    Ok(spec)
}

/// Holds a slug to what a filename, a route segment and a tool argument can all
/// carry.
///
/// A slug reaches storage as `<slug>.json` and a URL as a path segment, so
/// anything outside this set is a traversal question nobody should have to
/// think about twice.
///
/// # Errors
///
/// Returns [`OpenCompanyError::InvalidRequest`] when `raw` is empty, too long,
/// or carries anything but lowercase letters, digits and hyphens.
pub fn normalize_slug(raw: &str) -> Result<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err(invalid("a ledger needs a `slug`"));
    }
    if value.len() > 48
        || value.starts_with('-')
        || value.ends_with('-')
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(invalid(format!(
            "`{value}` is not a ledger slug; use lowercase letters, digits and hyphens, up to 48 \
             characters, not starting or ending with a hyphen"
        )));
    }
    Ok(value)
}

/// Holds a derived path to one plain Markdown file directly under
/// [`DERIVED_DIR`].
///
/// No nesting and no traversal: the folder's whole meaning is *nothing in here
/// is hand-written*, and a guard that has to reason about `derived/a/../../x`
/// is a guard with a hole in it. A declaration that names nothing gets
/// `derived/<SLUG>.md`, which is what a caller meant anyway.
fn normalize_derived(raw: &str, slug: &str) -> Result<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(format!(
            "{DERIVED_DIR}/{}.md",
            slug.to_ascii_uppercase().replace('-', "_")
        ));
    }
    let Some(name) = value.strip_prefix(&format!("{DERIVED_DIR}/")) else {
        return Err(invalid(format!(
            "`{value}` is not a derived path; every ledger renders into `{DERIVED_DIR}/<NAME>.md`, \
             the one folder nothing hand-writes"
        )));
    };
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name.ends_with(".md")
    {
        return Err(invalid(format!(
            "`{value}` is not a derived path; use `{DERIVED_DIR}/<NAME>.md` with no sub-folders"
        )));
    }
    Ok(value.to_string())
}

fn trimmed_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn first_duplicate<'a>(names: impl Iterator<Item = &'a String>) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for name in names {
        let lowered = name.to_ascii_lowercase();
        if seen.contains(&lowered) {
            return Some(name.clone());
        }
        seen.push(lowered);
    }
    None
}

pub(super) fn invalid(message: impl Into<String>) -> OpenCompanyError {
    OpenCompanyError::InvalidRequest(message.into())
}

#[cfg(test)]
#[path = "spec_test.rs"]
mod test;
