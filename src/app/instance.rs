//! The host's stable, public identity.
//!
//! A desktop client connects to several OpenCompany instances at once and has
//! to keep them apart — in its connection list, in its per-connection storage
//! keys, and in its caches. A URL cannot do that job: a host moves between
//! `localhost:8080`, a LAN address and a tunnel over one afternoon, and two
//! different hosts can share a URL over time. The client needs to ask "is this
//! the same instance I already know?" and get a durable answer.
//!
//! ## Random, not derived
//!
//! It would be easy to derive this from the hostname, the bind address, or the
//! company set. All three are wrong for the same reason: `/spec` is
//! **unauthenticated**, so anything derived is a fact about the deployment
//! handed to anyone who asks. A hostname names infrastructure; a bind address
//! names topology. Random bytes name nothing.
//!
//! Random also means two hosts restored from the same backup are the same
//! instance, and two fresh hosts never collide — neither of which a derived id
//! gets right.
//!
//! ## Not a secret
//!
//! It authenticates nothing. It is a name, and it is served to anonymous
//! callers by design, so it is stored world-readable and never compared against
//! anything. Do not grow a check that treats knowing it as proof of anything.

use std::path::Path;

use crate::server::users::token::{OsTokens, TokenSource};

/// The file under the instance home that holds the identity.
const INSTANCE_ID_FILE: &str = "instance-id";

/// How many random bytes back an instance id. 16 bytes = 128 bits, hex-encoded
/// to 32 characters: far past any collision concern, and short enough to read
/// out in a support conversation.
const INSTANCE_ID_BYTES: usize = 16;

/// Reads this instance's id, minting and persisting one on first boot.
///
/// Never fails. An unwritable home yields a fresh id for this process rather
/// than an error: the identity is a convenience for clients, and refusing to
/// serve because it could not be saved would take a whole host down over a
/// name. The cost of that degradation — a client seeing a "new" instance after
/// every restart — is logged rather than hidden, because a client silently
/// re-adding the same server is exactly the confusing symptom someone would
/// otherwise have to debug from the outside.
pub fn load_or_create(home: &Path) -> String {
    let path = home.join(INSTANCE_ID_FILE);

    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if is_well_formed(existing) {
            return existing.to_string();
        }
        // A truncated or hand-edited file. Replacing it is better than serving
        // something that is not an id, but say so: to a client this looks
        // exactly like a different server appearing at a familiar address.
        tracing::warn!(
            path = %path.display(),
            "instance-id file is not a well-formed id; minting a replacement"
        );
    }

    let minted = mint(&OsTokens);
    if let Err(error) = std::fs::write(&path, &minted) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not persist the instance id; this host will look like a new \
             instance to clients after every restart"
        );
    }
    minted
}

/// Reads an instance id from `home` without minting or writing one.
///
/// [`load_or_create`] is the boot path and is deliberately side-effecting: it
/// persists an id so the host has a stable identity. That is wrong for a caller
/// that only wants to *know* — the desktop lists data roots it is not running
/// (`src-tauri/src/local.rs`), and minting into each one would write to every
/// root an operator has ever stopped, purely as a side effect of drawing a list.
///
/// `None` when the root holds no id yet, or holds something that is not one.
pub fn peek(home: &Path) -> Option<String> {
    let existing = std::fs::read_to_string(home.join(INSTANCE_ID_FILE)).ok()?;
    let existing = existing.trim();
    is_well_formed(existing).then(|| existing.to_string())
}

/// Mints a fresh id from `src`.
fn mint(src: &dyn TokenSource) -> String {
    let mut bytes = [0u8; INSTANCE_ID_BYTES];
    src.fill(&mut bytes);
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Whether `value` is an id this module would have minted.
///
/// Deliberately strict. The alternative — accepting whatever the file holds —
/// means an empty file, a stray newline, or a log line accidentally redirected
/// over it all become this host's public identity.
fn is_well_formed(value: &str) -> bool {
    value.len() == INSTANCE_ID_BYTES * 2 && value.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod test {
    use super::*;

    struct Fixed(u8);
    impl TokenSource for Fixed {
        fn fill(&self, out: &mut [u8]) {
            out.fill(self.0);
        }
    }

    /// `peek` reads, and — the point of it existing — never writes.
    #[test]
    fn peeking_reports_an_id_without_creating_one() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(peek(dir.path()), None, "an empty root has no id to report");
        assert!(
            !dir.path().join(INSTANCE_ID_FILE).exists(),
            "peeking must not mint: the desktop peeks at roots it is not running"
        );

        let id = load_or_create(dir.path());
        assert_eq!(peek(dir.path()).as_deref(), Some(id.as_str()));
    }

    /// A hand-edited file is not an id, and `peek` says so rather than
    /// replacing it — replacing is `load_or_create`'s job, on the boot path.
    #[test]
    fn peeking_refuses_a_malformed_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(INSTANCE_ID_FILE), "not-an-id").unwrap();

        assert_eq!(peek(dir.path()), None);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(INSTANCE_ID_FILE)).unwrap(),
            "not-an-id",
            "peeking leaves the file alone"
        );
    }

    #[test]
    fn a_minted_id_is_hex_and_well_formed() {
        let id = mint(&Fixed(0xab));
        assert_eq!(id, "ab".repeat(INSTANCE_ID_BYTES));
        assert!(is_well_formed(&id));
    }

    #[test]
    fn an_id_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path());
        let second = load_or_create(dir.path());
        assert_eq!(first, second, "the id must be stable across boots");
        assert!(is_well_formed(&first));
    }

    #[test]
    fn two_homes_get_different_ids() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(load_or_create(a.path()), load_or_create(b.path()));
    }

    #[test]
    fn a_corrupt_file_is_replaced_rather_than_served() {
        let dir = tempfile::tempdir().unwrap();
        for junk in ["", "   ", "not-an-id", "zz", &"ab".repeat(64)] {
            std::fs::write(dir.path().join(INSTANCE_ID_FILE), junk).unwrap();
            let id = load_or_create(dir.path());
            assert!(is_well_formed(&id), "{junk:?} produced {id:?}");
            assert_ne!(id.trim(), junk.trim());
        }
    }

    #[test]
    fn an_unwritable_home_still_yields_an_id() {
        // A read-only or missing home must not take the host down over a name.
        let id = load_or_create(Path::new("/nonexistent-oc-instance-home"));
        assert!(is_well_formed(&id));
    }
}
