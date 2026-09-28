//! Issuing the *first* password for a company, from the host.
//!
//! # Why this exists
//!
//! Every way into a new company runs through a credential the deployment may
//! not be able to deliver (#1718):
//!
//! - `POST …/auth/password` needs a session, which is what we are trying to get.
//! - `POST …/users/{id}/password` and the invite routes need an existing
//!   **admin**, and on a first boot there is none.
//! - The magic link needs a mail transport. Its code is minted and stored
//!   *hashed*, so on a host with no transport the credential exists and is
//!   unreachable.
//! - The dev echo of that code is gated on [`AppConfig::is_local_only`], which
//!   is false for exactly the hosted deployment that has this problem.
//!
//! So a self-hosted company with no mail could not be signed into at all. The
//! console said as much — *"an admin can issue you one if you have none"* —
//! with nobody to ask.
//!
//! Two answers live here, for two different callers:
//!
//! - [`issue_password`] is the **host-side** one (`opencompany issue-password`).
//!   It is deliberately not reachable over HTTP: the authority it relies on is
//!   possession of the process and its storage, which an operator already has
//!   and a request never does. It issues a password only to an address that is
//!   *already* eligible — named in the manifest's `[users] admins`, or injected
//!   as the deployment's bootstrap admin — and cannot invent membership.
//! - [`claim_first_admin`] is the **console** one (`POST …/auth/claim`), for the
//!   person who just started a host and is looking at its sign-in screen with
//!   no way in. It is open exactly as long as the company has **no users at
//!   all**, and closes for good the moment the first one exists. Whoever
//!   reaches a fresh host first picks the admin login and its password — the
//!   same first-run claim every self-hosted product with a login screen makes,
//!   and the alternative was a shell command nobody running `docker compose
//!   up` had been told about. Where the deployment already named its first
//!   admin, only that address may claim; a stranger reaching a provisioned
//!   tenant first must not be able to take it.

use std::sync::Arc;

use crate::error::OpenCompanyError;
use crate::ports::generate_id;
use crate::ports::types::CompanyId;
use crate::ports::users::{UserRecord, UserRole, UserStatus, UserStore, normalize_email};
use crate::server::users::{password, token};

/// What [`issue_password`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    /// The address the password now belongs to, normalized.
    pub email: String,
    /// Whether the account was created, as opposed to an existing one updated.
    pub created: bool,
    /// Whether the holder must replace this password before doing anything else.
    pub must_change_password: bool,
}

/// The addresses a company admits without an invite record: its manifest
/// admins, plus the deployment's bootstrap admin when one is injected.
///
/// Both are the same grant, so they are one list. Shared with the HTTP path's
/// `bootstrap_admins` rather than re-derived, so the CLI cannot come to a
/// different answer than the login route about who is eligible.
pub fn standing_admins(manifest_admins: &[String], bootstrap_admin: Option<&str>) -> Vec<String> {
    let mut admins: Vec<String> = manifest_admins
        .iter()
        .map(|a| normalize_email(a))
        .filter(|email| !email.is_empty())
        .fold(Vec::new(), |mut admins, email| {
            if !admins.contains(&email) {
                admins.push(email);
            }
            admins
        });
    if let Some(email) = bootstrap_admin
        .map(normalize_email)
        .filter(|e| !e.is_empty())
        && !admins.contains(&email)
    {
        admins.push(email);
    }
    admins
}

/// The stores and company context used to issue a password.
///
/// Keeping these related inputs together also makes the host-side operation
/// harder to call with the wrong stores or company.
pub struct PasswordIssueContext<'a> {
    /// User persistence.
    pub users: &'a Arc<dyn UserStore>,
    /// Session persistence.
    pub sessions: &'a Arc<dyn crate::ports::sessions::SessionStore>,
    /// Login-code persistence.
    pub login_codes: &'a Arc<dyn crate::ports::login_codes::LoginCodeStore>,
    /// Company receiving the password.
    pub company: &'a CompanyId,
    /// Admin addresses declared by the company manifest.
    pub manifest_admins: &'a [String],
    /// Optional deployment-provided standing admin.
    pub bootstrap_admin: Option<&'a str>,
}

/// Sets `email`'s password in `company`, creating the account if the address is
/// eligible and has none.
///
/// `require_change` flags the account so the holder must replace the password
/// before doing anything else — the same treatment an admin-issued temporary
/// password gets, and the right default when the operator and the eventual
/// holder are different people.
pub async fn issue_password(
    context: PasswordIssueContext<'_>,
    email: &str,
    plaintext: &str,
    require_change: bool,
) -> Result<Issued, OpenCompanyError> {
    let PasswordIssueContext {
        users,
        sessions,
        login_codes,
        company,
        manifest_admins,
        bootstrap_admin,
    } = context;
    let email = normalize_email(email);
    if email.is_empty() {
        return Err(OpenCompanyError::InvalidRequest(
            "an email address is required".into(),
        ));
    }

    // Validated before anything is written, and against the address it will
    // belong to — `validate` refuses a password that contains its own email.
    password::validate(plaintext, &email)?;

    let existing = users.find_user_by_email(company, &email).await?;

    // Existing accounts may only be reset while retaining their administrative
    // role. A removed standing grant does not erase historical admin status.
    if let Some(existing) = existing.as_ref()
        && existing.role != UserRole::Admin
    {
        return Err(OpenCompanyError::InvalidRequest(format!(
            "{email} is not an admin account and cannot receive a host password reset"
        )));
    }

    // A suspended account cannot sign in at all — the password login path
    // refuses every non-active user — so committing a new password here would
    // only claim a success that can never be used. Refuse it outright instead
    // of persisting an unusable credential.
    if let Some(existing) = existing.as_ref()
        && existing.status != UserStatus::Active
    {
        return Err(OpenCompanyError::InvalidRequest(format!(
            "{email} is suspended and cannot receive a host password reset"
        )));
    }

    // Eligibility is only consulted when there is no account yet. An address
    // that already holds one keeps it even if the manifest later stops naming
    // them: removing someone is `status`, not a silent inability to reset.
    if existing.is_none() {
        let admins = standing_admins(manifest_admins, bootstrap_admin);
        if !admins.contains(&email) {
            return Err(OpenCompanyError::InvalidRequest(format!(
                "{email} is not a standing admin of `{}`, so there is no account to issue a \
                 password for. Add the address to the manifest's [users] admins, or set the \
                 deployment's bootstrap admin, and try again. This command makes an existing \
                 grant usable without mail; it does not create one.",
                company.as_ref()
            )));
        }
    }

    let hash = password::hash(&token::OsTokens, plaintext)?;
    let now = crate::ports::now_millis();
    let created = existing.is_none();

    let user = match existing {
        Some(mut user) => {
            user.password_hash = Some(hash);
            user.must_change_password = require_change;
            user.updated_at_millis = now;
            user
        }
        None => UserRecord {
            // The same id scheme the login path mints, so an account created
            // here is indistinguishable from one created by a magic link.
            id: generate_id(),
            email: email.clone(),
            display_name: None,
            avatar: None,
            // Eligibility above proved this address is a standing *admin*;
            // there is no other role this path can mint.
            role: UserRole::Admin,
            status: UserStatus::Active,
            password_hash: Some(hash),
            must_change_password: require_change,
            created_at_millis: now,
            last_seen_at_millis: None,
            updated_at_millis: now,
        },
    };

    // Revoke old credentials before the password commit. If either revocation
    // fails, no new password is persisted and the old credential state remains
    // the only usable state.
    if !created {
        sessions.delete_for_user(company, &user.id).await?;
    }
    login_codes.delete_for_email(company, &user.email).await?;
    users.upsert_user(company, &user).await?;
    // A newly minted account is the same materialization the login path
    // produces on redemption, so mark any outstanding invite redeemed the same
    // way — a manifest admin's bootstrapped invite record otherwise reads as
    // still pending beside an account that now exists. A bootstrap admin with
    // no invite record is a no-op.
    if created && let Some(mut invite) = users.find_invite_by_email(company, &email).await? {
        invite.accepted_at_millis = Some(now);
        users.upsert_invite(company, &invite).await?;
    }
    Ok(Issued {
        email,
        created,
        must_change_password: require_change,
    })
}

/// Why a first-admin claim was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimRefusal {
    /// Somebody already holds an account here, so there is no first admin left
    /// to claim. The ordinary sign-in is the way in.
    AlreadyClaimed,
    /// The deployment named its first admin (manifest or environment) and this
    /// is not that address.
    NotTheNamedAdmin,
}

/// Whether `company` still has no users, which is the one state in which
/// [`claim_first_admin`] is open.
pub async fn is_unclaimed(
    users: &Arc<dyn UserStore>,
    company: &CompanyId,
) -> Result<bool, OpenCompanyError> {
    Ok(users.list_users(company).await?.is_empty())
}

/// Mints the first admin of a company nobody has joined yet, with a password.
///
/// `standing` is what [`standing_admins`] returned for this company. When it
/// names anybody, `email` has to be one of them — the deployment decided who
/// owns this instance and a first-come claim must not overrule it. When it is
/// empty the address is the caller's to choose, and it is stored as typed
/// (lowercased and trimmed, like every login identity) with no check that it
/// is a mailbox: on a host with no mail transport there is nothing to send to,
/// and a plain username is a perfectly good login.
///
/// Refuses with [`ClaimRefusal::AlreadyClaimed`] the moment any user exists.
/// The check and the write are not one transaction, so two claims racing on an
/// empty company can both succeed; that window is a few milliseconds on a host
/// that has just booted, and the second claim is visible on the roster to the
/// first, which is the honest place for it to be.
pub async fn claim_first_admin(
    users: &Arc<dyn UserStore>,
    company: &CompanyId,
    standing: &[String],
    email: &str,
    plaintext: &str,
) -> Result<Result<UserRecord, ClaimRefusal>, OpenCompanyError> {
    // The same rule the manifest validator applies to `[users].admins`, so a
    // login that could not have been written there cannot be claimed here
    // either — in particular the `none`-mode owner's own `local:owner` key.
    if !crate::ports::users::is_usable_admin_email(email) {
        return Err(OpenCompanyError::InvalidRequest(
            "that is not a usable login — an email address or a single word".into(),
        ));
    }
    let email = normalize_email(email);
    if !is_unclaimed(users, company).await? {
        return Ok(Err(ClaimRefusal::AlreadyClaimed));
    }
    if !standing.is_empty() && !standing.contains(&email) {
        return Ok(Err(ClaimRefusal::NotTheNamedAdmin));
    }
    password::validate(plaintext, &email)?;
    let hash = password::hash(&token::OsTokens, plaintext)?;
    let now = crate::ports::now_millis();
    let user = UserRecord {
        id: generate_id(),
        email: email.clone(),
        display_name: None,
        avatar: None,
        role: UserRole::Admin,
        status: UserStatus::Active,
        password_hash: Some(hash),
        // They chose this password themselves a moment ago; there is nothing
        // to replace.
        must_change_password: false,
        created_at_millis: now,
        last_seen_at_millis: Some(now),
        updated_at_millis: now,
    };
    users.upsert_user(company, &user).await?;
    if let Some(mut invite) = users.find_invite_by_email(company, &email).await? {
        invite.accepted_at_millis = Some(now);
        users.upsert_invite(company, &invite).await?;
    }
    Ok(Ok(user))
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
