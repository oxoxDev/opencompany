//! The [`UserStore`] port: the company's directory of human collaborators.
//!
//! Users are the people who work alongside the company's agents — they read the
//! console and talk to desks in chat. They are not billing subjects: the
//! platform's Node backend owns accounts and money, and nothing here knows about
//! either. A user exists only inside one company, which is why every method is
//! keyed by [`CompanyId`].
//!
//! Access is invite-only. An [`InviteRecord`] is an admin's standing permission
//! for one email address to become a [`UserRecord`]; the address cannot log in
//! before that invite exists, and redeeming it is what mints the user. Both live
//! behind one port because they share the email keyspace — "invited" and
//! "joined" are two states of the same address and must stay consistent.
//!
//! Credential material is deliberately *not* here: session tokens live in
//! [`SessionStore`](crate::ports::SessionStore) and login codes in
//! [`LoginCodeStore`](crate::ports::LoginCodeStore), so they can carry their own
//! expiry/purge rules and stay out of any export path.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::ports::types::CompanyId;

/// What a user is allowed to do inside their company.
///
/// Deliberately two-valued: the product need is "who may invite others", not a
/// permission matrix. Anything finer belongs in a later, explicit design.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// May invite and remove other users, in addition to everything a member can do.
    Admin,
    /// May use the company — read the console, chat with desks.
    #[default]
    Member,
}

impl UserRole {
    /// Whether this role may invite, revoke, and remove other users.
    pub fn may_administer(&self) -> bool {
        matches!(self, UserRole::Admin)
    }
}

/// Whether a user may currently authenticate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    /// Normal, may log in.
    #[default]
    Active,
    /// Retained for attribution, but refused at login and on every request.
    Suspended,
}

/// One human collaborator in a company.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    /// Stable id for the user within the company. Used as [`Actor::id`](crate::ports::types::Actor)
    /// when attributing this user's chat messages, so it must outlive the email.
    pub id: String,
    /// The user's **login identity key**, unique within the company.
    ///
    /// Named `email` because that is all it ever held before wallet and
    /// device-only sign-in existed, and renaming it would rewrite the column in
    /// three storage backends to say the same thing. What it holds is decided by
    /// the company's [`AuthMode`](crate::app::config::AuthMode) and is always the
    /// output of [`LoginIdentity::key`]: a normalized address in `email` mode,
    /// `wallet:<base58>` in `wallet` mode, `local:owner` in `none` mode.
    ///
    /// Read it as [`Self::identity`] rather than as an address. In particular,
    /// mail paths must go through [`LoginIdentity::mailbox`], which is `None`
    /// for every identity that has no mailbox to send to.
    pub email: String,
    /// An optional human-readable name for the console to render.
    ///
    /// `None` means **this person has not named themselves**, and is never
    /// filled in with a guess: the console derives a readable name from the
    /// login identity at render time (`steven.enamakel@…` reads as "Steven
    /// Enamakel"), which keeps a guess looking like a guess and leaves this
    /// field meaning "what they actually chose". Storing the derivation would
    /// make the two indistinguishable — and would freeze a guess about somebody
    /// into the record the moment they first signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The face this person wears, when they have chosen one — a
    /// `tiny:<flavour>` mascot or a `blob:<nodeId>` upload
    /// (`docs/spec/runtime/avatars.md`).
    ///
    /// `None` is the same "nobody has chosen" state [`Self::display_name`]
    /// carries, and for the same reason: the console draws the mascot it hashes
    /// from this user's id, so a person always has a face, and clearing this
    /// field is how they get that default back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// What the user may do.
    pub role: UserRole,
    /// Whether the user may currently authenticate.
    pub status: UserStatus,
    /// Argon2id PHC hash of the user's password, if they have set one.
    ///
    /// `None` means magic-link only — the common case, and the state every user
    /// starts in. Never the password itself: see
    /// [`password`](crate::server::users::password). This field must never be
    /// serialized into a route response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
    /// Whether the user must replace their password before doing anything
    /// else, set when an admin issues a temporary one.
    ///
    /// A real boundary, not a hint: the auth extractors refuse a flagged
    /// session with `403 password_change_required` everywhere except
    /// set-password, logout, and `me`. An admin who resets a password knows it
    /// and conveys it over a channel they do not control, so a session opened
    /// with one is good for exactly one thing.
    #[serde(default)]
    pub must_change_password: bool,
    /// Epoch-millis timestamp of when the user redeemed their invite.
    pub created_at_millis: u64,
    /// Epoch-millis timestamp of the user's most recent **sign-in**.
    ///
    /// Stamped when a session is minted — by link or by password — not on every
    /// authenticated request. Tracking activity would cost a store write per
    /// call, which is not worth knowing someone was here a minute ago.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at_millis: Option<u64>,
    /// Epoch-millis timestamp of the last update to this record.
    pub updated_at_millis: u64,
}

impl UserRecord {
    /// How this user signs in, parsed from their [`Self::email`] key.
    pub fn identity(&self) -> LoginIdentity {
        LoginIdentity::parse(&self.email)
    }

    /// The mailbox this user can be written to, if they have one.
    pub fn mailbox(&self) -> Option<String> {
        self.identity().mailbox().map(str::to_string)
    }

    /// What to call this person on screen: the name they chose, else one
    /// derived from their login identity, else `None`.
    ///
    /// The fallback is [`derive_display_name`] — see there for why a derived
    /// name is not written into [`Self::display_name`].
    pub fn display_label(&self) -> Option<String> {
        self.display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| derive_display_name(&self.email))
    }
}

/// A readable name guessed from a login identity — `steven.enamakel@acme.com`
/// reads as "Steven Enamakel".
///
/// # Why this is derived and never stored
///
/// A person who has not named themselves still has to be called something on
/// every surface that shows them, and the honest options are the raw address,
/// nothing, or a guess. The raw address is refused elsewhere on this page's own
/// rule — being in a company should not hand everyone your mailbox — and
/// nothing leaves a chat message attributed to a blank.
///
/// So: a guess, made at render time. Writing it into
/// [`UserRecord::display_name`] instead would be the tempting version and is
/// wrong in a way that does not show up until later — the field would no longer
/// mean "what this person chose", so nothing could tell a guess from a decision,
/// and a person who never touched their profile would look like one who had. It
/// would also freeze whatever the guess was on the day they first signed in.
///
/// # What it will and will not guess
///
/// Only the **local part**, split on the separators people actually use, with
/// each word capitalised. The domain is dropped: it is the half that identifies
/// the mailbox rather than the person.
///
/// `None` for an identity with no name in it to find — a wallet key, the local
/// owner of a company with no sign-in, or a local part with no letters. `None`
/// means "cannot say", and a caller should render something honest (an initial,
/// a role noun) rather than a guess this function refused to make.
pub fn derive_display_name(identity_key: &str) -> Option<String> {
    let LoginIdentity::Email(address) = LoginIdentity::parse(identity_key) else {
        // A base58 public key and `local:owner` are identities, not names.
        // Capitalising either would produce something that looks like a name and
        // is not one, which is worse than admitting there is nothing here.
        return None;
    };
    let local = address.split('@').next().unwrap_or_default();
    // `steven+acme@…` is one mailbox with a routing tag; the tag is plumbing.
    let local = local.split('+').next().unwrap_or(local);
    let words: Vec<String> = local
        .split(['.', '_', '-'])
        .filter(|word| !word.is_empty())
        .map(capitalise)
        .collect();
    if words.is_empty() || !words.iter().any(|w| w.chars().any(char::is_alphabetic)) {
        return None;
    }
    Some(words.join(" "))
}

/// Upper-cases the first character and leaves the rest alone.
///
/// The rest is left alone deliberately: lower-casing it would turn `McDonald`
/// into `Mcdonald` and `JPMorgan` into `Jpmorgan`, and a local part that already
/// carries capitals is one somebody chose to write that way.
fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// An admin's standing permission for one email address to join the company.
///
/// An invite is not a credential — it grants no access on its own and is safe to
/// list back to admins. It only makes the address *eligible* to request a login
/// code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteRecord {
    /// Stable id for the invite within the company.
    pub id: String,
    /// The invited email address, already normalized by [`normalize_email`].
    /// Unique within the company.
    pub email: String,
    /// The role the user will be created with when they redeem this invite.
    pub role: UserRole,
    /// Who sent the invite, as an [`Actor`](crate::ports::types::Actor) id. The
    /// operator token invites as `"operator"`.
    pub invited_by: String,
    /// Epoch-millis timestamp of when the invite was created.
    pub created_at_millis: u64,
    /// Epoch-millis timestamp after which the invite is no longer redeemable.
    pub expires_at_millis: u64,
    /// Epoch-millis timestamp of redemption; `None` while still outstanding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at_millis: Option<u64>,
    /// Epoch-millis timestamp of when the invite mail was accepted by the
    /// transport; `None` if no mail was sent.
    ///
    /// `None` is the honest answer for three different situations, and the
    /// console says so rather than implying delivery: the host has no mail
    /// transport wired, the send was attempted and failed, or the record
    /// predates invite mail existing at all. Every store persists invites as a
    /// JSON blob, so `serde(default)` is the whole migration — an older row
    /// loads as `None`, which is true of it.
    ///
    /// It records that the transport *accepted* the message, which is the
    /// furthest thing this process can honestly claim to know. A bounce after
    /// that point happens somewhere this code cannot see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified_at_millis: Option<u64>,
}

impl InviteRecord {
    /// Whether this invite can still be redeemed at `now_millis`.
    pub fn is_redeemable(&self, now_millis: u64) -> bool {
        self.accepted_at_millis.is_none() && now_millis < self.expires_at_millis
    }
}

/// Normalizes an email address into its canonical storage/lookup form.
///
/// Trims surrounding whitespace and lowercases. The local part of an address is
/// technically case-sensitive per RFC 5321, but no mail provider in practice
/// treats it so, and matching case-sensitively here would let one person hold
/// two accounts (and two invites) for what is really one mailbox. Every write
/// and every lookup must go through this, or the uniqueness index is a lie.
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Whether `raw` is usable as a login — a `[users].admins` entry, or the
/// address the first admin claims.
///
/// The one definition, so the manifest validator, the first-run wizard and the
/// claim route cannot disagree about which logins are acceptable. Pinned by
/// `tests/fixtures/setup-admin-email.json`, which the console's own test reads —
/// the console cannot call this, so a fixture is what keeps its
/// re-implementation honest.
///
/// Deliberately loose. It is not a mail-server-grade check and must not become
/// one, and it does **not** demand an `@`: on a host with no mail transport the
/// login is a username, not a mailbox, and there is nothing to send to. The
/// demands are only that the entry survives [`normalize_email`] as something
/// non-empty with no whitespace inside it, and that [`LoginIdentity::parse`]
/// reads it as an email rather than as the `wallet:` or `local:` scheme — an
/// entry that normalized to `local:owner` would be stored under the `none`-mode
/// local owner's own key and become a different principal than the one meant.
pub fn is_usable_admin_email(raw: &str) -> bool {
    let normalized = normalize_email(raw);
    !normalized.is_empty()
        && !normalized.chars().any(char::is_whitespace)
        && matches!(LoginIdentity::parse(&normalized), LoginIdentity::Email(_))
}

/// The scheme prefix marking a [`LoginIdentity::Wallet`] key.
const WALLET_PREFIX: &str = "wallet:";

/// The scheme prefix marking a [`LoginIdentity::Local`] key.
const LOCAL_PREFIX: &str = "local:";

/// The one local identity a `none`-mode company has.
const LOCAL_OWNER: &str = "owner";

/// How a person proves who they are, and therefore what
/// [`UserRecord::email`] holds.
///
/// The stores treat that column as one opaque, unique-per-company identity key
/// — every backend indexes it, the invite keyspace shares it, and suspension,
/// session revocation and removal all key off the [`UserRecord::id`] it
/// resolves to. Which *kind* of identity fills it is decided by the company's
/// [`AuthMode`](crate::app::config::AuthMode), so the key carries its own
/// scheme rather than leaving three keyspaces to collide:
///
/// | Mode | Key | Example |
/// |---|---|---|
/// | `email` | the address, verbatim | `ada@example.com` |
/// | `wallet` | `wallet:` + base58 Ed25519 public key | `wallet:7xKX…` |
/// | `none` | `local:owner` | `local:owner` |
///
/// The prefixes are what make this safe to store in one column, but a prefix
/// alone is not: `wallet:ada@example.com` and `local:owner@example.com` are
/// both keys [`normalize_email`] can produce, since it only lowercases and
/// trims. [`Self::parse`] therefore trusts a prefix only when the remainder is
/// actually of that scheme — a valid base58 string for `wallet:`, and exactly
/// `owner` for `local:` — so an email that happens to start with one of these
/// words still parses back as the email it is.
///
/// Normalization differs by scheme and that difference is load-bearing:
/// [`normalize_email`] lowercases, and lowercasing a base58 address would
/// silently map distinct wallets onto one key. Build the key through
/// [`Self::key`] and nothing has to remember which rule applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginIdentity {
    /// A mailbox, already normalized by [`normalize_email`].
    Email(String),
    /// A base58-encoded Ed25519 wallet public key, case preserved.
    Wallet(String),
    /// The single implicit owner of a company that has no login at all.
    Local,
}

impl LoginIdentity {
    /// Parses a stored [`UserRecord::email`] key back into its scheme.
    ///
    /// An unprefixed key is an email, which is what every record written before
    /// wallet and local identities existed holds — so this is the whole
    /// migration.
    pub fn parse(key: &str) -> Self {
        // Both prefixes are checked for an *exact* scheme before being trusted,
        // not merely a string prefix: `normalize_email` only lowercases and
        // trims, so `wallet:ada@example.com` and `local:owner@example.com` are
        // both keys a real invite can normalize an email address into. Without
        // this, they would misparse as a wallet or the local owner instead of
        // the email they are — silently breaking mail delivery and, for
        // `local:owner@example.com`, granting local-owner semantics to an
        // email address. The wallet remainder is checked with the same
        // `decode_wallet_address` the login and invite routes use — not merely
        // "is this base58" — so a short base58 string that happens to follow
        // `wallet:` (which a base58-alphabet email local part could produce)
        // does not misclassify as a wallet either; it has to actually be the
        // 32 bytes an Ed25519 public key is.
        if let Some(address) = key.strip_prefix(WALLET_PREFIX)
            && decode_wallet_address(address).is_ok()
        {
            return Self::Wallet(address.to_string());
        }
        if key.strip_prefix(LOCAL_PREFIX) == Some(LOCAL_OWNER) {
            return Self::Local;
        }
        Self::Email(key.to_string())
    }

    /// The normalized storage/lookup key for this identity.
    pub fn key(&self) -> String {
        match self {
            Self::Email(address) => normalize_email(address),
            Self::Wallet(address) => format!("{WALLET_PREFIX}{}", normalize_wallet(address)),
            Self::Local => format!("{LOCAL_PREFIX}{LOCAL_OWNER}"),
        }
    }

    /// The mailbox to send to, and `None` for every identity that has none.
    ///
    /// Mail paths must ask for an address through this rather than reading
    /// [`UserRecord::email`] directly: the column is an identity key, and a
    /// `wallet:7xKX…` handed to an SMTP transport is a bug that only shows up in
    /// a bounce log. An email identity with no `@` is a plain username — a
    /// login on a host with no mail — and has no mailbox either.
    pub fn mailbox(&self) -> Option<&str> {
        match self {
            Self::Email(address) if address.contains('@') => Some(address),
            Self::Email(_) | Self::Wallet(_) | Self::Local => None,
        }
    }

    /// What the console shows for this identity: the address, the wallet, or a
    /// fixed label for the local owner.
    pub fn label(&self) -> String {
        match self {
            Self::Email(address) => address.clone(),
            Self::Wallet(address) => address.clone(),
            Self::Local => "This device".to_string(),
        }
    }
}

/// Normalizes a wallet address into its canonical storage/lookup form.
///
/// Trims surrounding whitespace and **nothing else**. Base58 is case-sensitive,
/// so unlike [`normalize_email`] this must not fold case: `7xKXtg2C` and
/// `7XKXTG2C` decode to different keys, and treating them as one address would
/// let a signature verified against one wallet mint a session for another.
///
/// This does not validate — see [`decode_wallet_address`].
pub fn normalize_wallet(raw: &str) -> String {
    raw.trim().to_string()
}

/// Decodes a base58 wallet address into the 32-byte Ed25519 public key a
/// signature is checked against.
///
/// The same function validates a manifest's `[users].wallets` entry and admits a
/// sign-in attempt, so an address the manifest accepted can always be verified
/// against — an operator learns about a typo from `opencompany check` rather
/// than from a wallet that can never sign in.
///
/// The error text is deliberately prosumer-facing: it is rendered by manifest
/// validation. It is *not* rendered on the login path, which answers every
/// failure identically.
pub fn decode_wallet_address(raw: &str) -> Result<[u8; 32]> {
    use crate::error::OpenCompanyError;

    let address = normalize_wallet(raw);
    let bytes = bs58::decode(&address).into_vec().map_err(|_| {
        OpenCompanyError::InvalidRequest(format!(
            "`{address}` is not a base58 wallet address — expected a Solana-style public key"
        ))
    })?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        OpenCompanyError::InvalidRequest(format!(
            "`{address}` decodes to {} bytes, not the 32 an Ed25519 public key has",
            bytes.len()
        ))
    })
}

/// The company's durable user directory. Company A's users MUST be invisible to
/// company B.
///
/// Implementations must enforce that `email` is unique within a company for
/// users and for invites (independently — an outstanding invite and an existing
/// user may briefly share an address during redemption). Lookups by email are on
/// the login hot path and must be indexed, not scanned.
#[async_trait]
pub trait UserStore: Send + Sync {
    /// Lists every user in the company, most-recently-created first.
    async fn list_users(&self, company: &CompanyId) -> Result<Vec<UserRecord>>;
    /// Fetches one user by id.
    async fn get_user(&self, company: &CompanyId, id: &str) -> Result<Option<UserRecord>>;
    /// Fetches one user by normalized email. The caller must pass the output of
    /// [`normalize_email`].
    async fn find_user_by_email(
        &self,
        company: &CompanyId,
        email: &str,
    ) -> Result<Option<UserRecord>>;
    /// Inserts or replaces a user by id.
    async fn upsert_user(&self, company: &CompanyId, user: &UserRecord) -> Result<()>;
    /// Deletes a user by id; returns whether one was removed.
    async fn delete_user(&self, company: &CompanyId, id: &str) -> Result<bool>;

    /// Lists every invite in the company, most-recently-created first.
    async fn list_invites(&self, company: &CompanyId) -> Result<Vec<InviteRecord>>;
    /// Fetches one invite by normalized email. The caller must pass the output
    /// of [`normalize_email`].
    async fn find_invite_by_email(
        &self,
        company: &CompanyId,
        email: &str,
    ) -> Result<Option<InviteRecord>>;
    /// Inserts or replaces an invite by id.
    async fn upsert_invite(&self, company: &CompanyId, invite: &InviteRecord) -> Result<()>;
    /// Stamps [`InviteRecord::notified_at_millis`] on an invite that still
    /// exists, leaving every other field alone. Returns whether one was
    /// updated.
    ///
    /// Deliberately narrower than [`UserStore::upsert_invite`], and the
    /// narrowness is the point. The stamp is written *after* the invite mail
    /// leaves, which is a network round trip — long enough for an admin who
    /// mistyped the address to revoke the invite in the meantime. Writing the
    /// pre-send record back through an upsert would recreate the row they just
    /// revoked, silently restoring an address to the allowlist. A no-op is the
    /// correct outcome there: the revocation stands, and nothing claims a mail
    /// landed for a grant that no longer exists.
    async fn mark_invite_notified(
        &self,
        company: &CompanyId,
        id: &str,
        at_millis: u64,
    ) -> Result<bool>;
    /// Deletes an invite by id; returns whether one was removed.
    async fn delete_invite(&self, company: &CompanyId, id: &str) -> Result<bool>;
}

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;
