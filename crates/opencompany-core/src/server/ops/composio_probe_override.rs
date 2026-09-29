use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

type Outcome = Result<(), String>;

fn map() -> &'static Mutex<HashMap<String, Outcome>> {
    static MAP: OnceLock<Mutex<HashMap<String, Outcome>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn set(company: &str, outcome: Outcome) {
    map()
        .lock()
        .expect("composio probe override")
        .insert(company.to_string(), outcome);
}

/// Drops one scope's forced answer.
///
/// The map is one process-wide `Mutex<HashMap>` rather than a thread-local, so
/// an entry outlives the test that wrote it — which matters for a scope that is
/// not a company id and so is not unique per test. `"setup"` is exactly that:
/// first-run has no company to key on, so every test of the setup check shares
/// one slot, and a forced answer left behind would answer a later one.
pub(crate) fn clear(company: &str) {
    map()
        .lock()
        .expect("composio probe override")
        .remove(company);
}

pub(super) fn get(company: &str) -> Option<Outcome> {
    map()
        .lock()
        .expect("composio probe override")
        .get(company)
        .cloned()
}
