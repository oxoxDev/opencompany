//! The ACP harness: a company turn served by an **external** agent over the
//! Agent Client Protocol.
//!
//! Gated behind the `acp` feature because nothing in a default build can reach
//! it — the endpoint that would drive it lives behind the same feature, and
//! `/acp` is a reserved prefix that 404s without it. Compiling it
//! unconditionally meant a surface that no lane ran and no route served (issue
//! #475).
//!
//! The transport is deliberately *not* here: a subprocess over stdio for the
//! desktop and a WebSocket for a runner belong to their own crates, so
//! [`run_turn`] defines an [`AcpAgent`](run_turn::AcpAgent) port and folds
//! whatever it reports. The same inversion the storage ports use.

#[cfg(feature = "acp")]
pub mod run_turn;
