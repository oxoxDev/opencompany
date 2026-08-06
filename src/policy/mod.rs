//! Approval policy: the manifest-`[policy]`-driven [`ApprovalGate`].
//!
//! The [`gate`] module implements the default
//! [`ApprovalGate`](crate::ports::ApprovalGate) that evaluates emitted effects
//! against a company's declared policy and holds the in-memory approval queue.
//!
//! The [`consequence`] module declares, once, what every tool can reach — the
//! single source both approval questions read ("may this run unattended?" and
//! "may an operator grant it standing?"). It is always compiled, because the
//! standing-grant rule is enforced in the default build (the mint path and the
//! console card) while the tool policy that parks calls compiles only under the
//! `openhuman` feature.

pub mod consequence;
pub mod gate;

pub use consequence::{Consequence, Reach, Standing, consequence_of};
pub use gate::{DEFAULT_TTL_MILLIS, ManifestApprovalGate};
