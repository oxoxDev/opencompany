//! Shared scaffolding for this crate's integration targets.
//!
//! Declared with `mod support;` from each `tests/*.rs` that needs it; not a
//! test target itself (no `[[test]]` entry names it), so nothing here runs
//! on its own and `assert-integration-targets-run.sh` never looks for it.

#![allow(dead_code)]

pub mod script_model;
