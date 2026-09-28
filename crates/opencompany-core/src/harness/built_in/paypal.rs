//! The agent-facing bridge for PayPal (issue #789): two read tools over
//! [`crate::paypal::api`].
//!
//! Same shape as [`crate::harness::chargebee`] — per-company credentials from
//! that company's [`SecretStore`], resolved at roster-build time, wired only on
//! an explicit `paypal` grant and only when a credential resolves.
//!
//! # Both tools are read-only, and that is the whole surface
//!
//! #789 lists `send_payment` as optional and requires a scoping decision before
//! implementation, so nothing here moves money. Both tools are therefore
//! [`PermissionLevel::ReadOnly`] and never park: asking what the balance is
//! should not need an approval click, and there is no write to guard.

use std::sync::Arc;

use crate::company::paypal::{
    CLIENT_ID_SECRET, CLIENT_SECRET_SECRET, ENVIRONMENT_SECRET, PaypalEnvironment,
};
use crate::ports::SecretStore;
use crate::ports::types::CompanyId;

/// One company's resolved PayPal connection.
#[derive(Clone, Debug)]
pub struct TenantPaypal {
    #[cfg_attr(not(feature = "paypal"), allow(dead_code))]
    config: crate::paypal::PaypalConfig,
}

impl TenantPaypal {
    /// Resolves a company's PayPal credentials from its secret store.
    ///
    /// `Ok(None)` unless BOTH halves are present: a client id with no secret
    /// cannot obtain a token, and half a credential should wire no tools rather
    /// than tools that fail on first use.
    ///
    /// A store **read failure** is an `Err`, not `Ok(None)` — see
    /// [`crate::harness::chargebee::TenantChargebee::resolve`] for why the two
    /// must stay distinguishable.
    pub async fn resolve(
        secrets: &Arc<dyn SecretStore>,
        company: &CompanyId,
    ) -> crate::error::Result<Option<Self>> {
        let read = async |key: &str| -> crate::error::Result<Option<String>> {
            Ok(secrets
                .get(company, key)
                .await?
                .map(|value| value.0.trim().to_string())
                .filter(|value| !value.is_empty()))
        };
        let (Some(client_id), Some(client_secret)) = (
            read(CLIENT_ID_SECRET).await?,
            read(CLIENT_SECRET_SECRET).await?,
        ) else {
            return Ok(None);
        };
        // An unset environment is sandbox, matching `PaypalEnvironment::parse`:
        // the safe default is reading fake money, never moving real money.
        let environment = read(ENVIRONMENT_SECRET)
            .await?
            .map(|raw| PaypalEnvironment::parse(&raw))
            .unwrap_or_default();

        Ok(Some(Self {
            config: crate::paypal::PaypalConfig {
                client_id,
                client_secret,
                environment,
            },
        }))
    }

    /// Which PayPal environment this company is pointed at. Never the credential.
    pub fn environment(&self) -> PaypalEnvironment {
        self.config.environment
    }

    /// A stable hash of the connection, for the roster staleness check.
    ///
    /// Covers the environment as well as both halves of the credential: moving
    /// a company from sandbox to live with the same keys must rebuild, or its
    /// agents keep reading the wrong world's balance until a restart.
    pub fn fingerprint(config: &Option<TenantPaypal>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match config {
            None => 0u8.hash(&mut hasher),
            Some(c) => {
                1u8.hash(&mut hasher);
                c.config.client_id.hash(&mut hasher);
                c.config.client_secret.hash(&mut hasher);
                c.config.environment.as_str().hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

#[cfg(feature = "paypal")]
pub use live::paypal_tools;

#[cfg(feature = "paypal")]
mod live {
    use super::*;

    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::{Value, json};

    use crate::paypal::api;
    use crate::paypal::client::PaypalClient;

    use openhuman_core as oh;
    use tinytools::{PermissionLevel, Tool, ToolResult};

    /// Builds the per-company PayPal tools over a resolved connection.
    pub fn paypal_tools(config: &TenantPaypal) -> Vec<Box<dyn Tool>> {
        let config = Arc::new(config.clone());
        vec![
            Box::new(WalletBalanceTool(Arc::clone(&config))),
            Box::new(ListTransactionsTool(config)),
        ]
    }

    /// Builds the client for a call about to be made.
    fn client(config: &TenantPaypal) -> crate::error::Result<PaypalClient> {
        PaypalClient::new(config.config.clone())
    }

    /// Renders a result, or the failure as text the agent can act on.
    fn render<T: serde::Serialize>(what: &str, outcome: crate::error::Result<T>) -> ToolResult {
        match outcome {
            Ok(value) => match serde_json::to_string_pretty(&value) {
                Ok(text) => ToolResult::success(text),
                Err(e) => {
                    ToolResult::error(format!("{what} succeeded but could not be rendered: {e}"))
                }
            },
            Err(e) => ToolResult::error(format!("{what} failed: {e}")),
        }
    }

    pub struct WalletBalanceTool(Arc<TenantPaypal>);

    #[async_trait]
    impl Tool for WalletBalanceTool {
        fn name(&self) -> &str {
            "paypal_get_wallet_balance"
        }

        fn description(&self) -> &str {
            "Fetch the current PayPal account balance, per currency. Returns the available and \
             withheld amounts as exact decimal strings — report them verbatim rather than \
             rounding or recomputing."
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object", "additionalProperties": false, "properties": {}})
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, _args: Value) -> Result<ToolResult> {
            let client = match client(&self.0) {
                Ok(client) => client,
                Err(e) => return Ok(ToolResult::error(format!("paypal client: {e}"))),
            };
            tracing::info!(
                environment = self.0.environment().as_str(),
                "[paypal] get_wallet_balance"
            );
            Ok(render(
                "paypal_get_wallet_balance",
                api::get_wallet_balance(&client).await,
            ))
        }
    }

    pub struct ListTransactionsTool(Arc<TenantPaypal>);

    #[async_trait]
    impl Tool for ListTransactionsTool {
        fn name(&self) -> &str {
            "paypal_list_transactions"
        }

        fn description(&self) -> &str {
            "List PayPal transactions between two dates. PayPal publishes on a delay of up to 3 \
             hours, so a window ENDING today is fine but one STARTING today usually has no data \
             and is rejected — start at least one day back. The window must span no more than 31 \
             days. To answer 'was I paid recently?', ask for the last 7 days rather than today."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "required": ["start_date", "end_date"],
                "additionalProperties": false,
                "properties": {
                    "start_date": {
                        "type": "string",
                        "description": "ISO 8601, e.g. 2026-08-01T00:00:00Z. At most 31 days before end_date."
                    },
                    "end_date": {
                        "type": "string",
                        "description": "ISO 8601, e.g. 2026-08-13T23:59:59Z."
                    },
                    "page_size": {"type": "integer", "minimum": 1, "maximum": 500}
                }
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let client = match client(&self.0) {
                Ok(client) => client,
                Err(e) => return Ok(ToolResult::error(format!("paypal client: {e}"))),
            };
            let start = args.get("start_date").and_then(Value::as_str).unwrap_or("");
            let end = args.get("end_date").and_then(Value::as_str).unwrap_or("");
            let page_size = args.get("page_size").and_then(Value::as_i64);
            Ok(render(
                "paypal_list_transactions",
                api::list_transactions(&client, start, end, page_size).await,
            ))
        }
    }
}

#[cfg(all(test, feature = "paypal"))]
#[path = "paypal_tests.rs"]
mod tests;
