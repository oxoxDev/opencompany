use std::cell::RefCell;
use std::collections::HashMap;

use async_trait::async_trait;

use crate::company::inference::probe::{ProbeClass, ProbeFailure};

pub(crate) type Outcome = std::result::Result<Vec<String>, ProbeClass>;

thread_local! {
    static MAP: RefCell<HashMap<String, Outcome>> = RefCell::new(HashMap::new());
}

pub(crate) fn set(company: &str, outcome: Outcome) {
    MAP.with(|map| {
        map.borrow_mut().insert(company.to_string(), outcome);
    });
}

pub(super) fn get(company: &str) -> Option<Outcome> {
    MAP.with(|map| map.borrow().get(company).cloned())
}

pub(super) struct Forced(pub(super) Outcome);

#[async_trait]
impl crate::company::company_key::InferenceProber for Forced {
    async fn probe(
        &self,
        _base_url: &str,
        _key: &str,
    ) -> std::result::Result<Vec<String>, ProbeFailure> {
        match &self.0 {
            Ok(ids) => Ok(ids.clone()),
            Err(class) => Err(ProbeFailure {
                class: *class,
                raw: "forced".to_string(),
                truncated: false,
            }),
        }
    }
}
