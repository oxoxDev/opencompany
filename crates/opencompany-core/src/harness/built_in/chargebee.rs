//! The agent-facing bridge for Chargebee billing (issue #788): five tools over
//! the operations in [`crate::chargebee::api`].
//!
//! # Why the credential is per company and resolved late
//!
//! Two companies on one host bill two different Chargebee sites, so a
//! process-wide key could only ever be wrong for one of them. The API key and
//! site identifier are read from **that company's** [`SecretStore`], written by
//! the console's Billing settings (#527). No environment variable is consulted.
//!
//! Resolution happens at roster-build time and is folded into the harness
//! fingerprint, so an operator who sets or rotates the key in the console gets
//! it on the next turn rather than after a restart — the same contract Composio
//! has.
//!
//! # Fail closed
//!
//! Tools are wired only when a company **explicitly** grants `chargebee` *and*
//! both secrets resolve. A catch-all `*` does not confer it: these tools move
//! money and send invoices to real people, so they are opted into by name rather
//! than ridden in on a wildcard set for file and shell tools. A grant with no
//! credential wires nothing and warns — never a borrowed identity.
//!
//! # Approval
//!
//! `chargebee_send_invoice` and `chargebee_create_customer` write to a billing
//! system a real customer sees, so they are [`PermissionLevel::Execute`] and
//! park through the harness approval policy. The three read tools are
//! [`PermissionLevel::ReadOnly`] and never park — asking "has Alan paid?"
//! should not need a click.

use std::sync::Arc;

use crate::chargebee::types::{API_KEY_SECRET, ChargebeeConfig, SITE_SECRET};
use crate::ports::SecretStore;
use crate::ports::types::CompanyId;

/// One company's resolved Chargebee connection.
#[derive(Clone, Debug)]
pub struct TenantChargebee {
    config: ChargebeeConfig,
}

impl TenantChargebee {
    /// Resolves a company's Chargebee credentials from its secret store.
    ///
    /// `Ok(None)` when either half is missing. Both are required and the pair is
    /// meaningless apart: a site with no key cannot be called, and a key pointed
    /// at the wrong site fails in a way that reads like a bad key.
    ///
    /// A store **read failure** is an `Err`, not `Ok(None)`. Collapsing the two
    /// would make an unhealthy secret store indistinguishable from "no
    /// credential configured", and the caller's response to those differs
    /// completely: absence should wire no tools, while a transient read error
    /// should keep the connection it already had. Deciding that here, rather
    /// than at the caller, is what makes the choice visible — see
    /// `HarnessPool::resolve_chargebee`.
    pub async fn resolve(
        secrets: &Arc<dyn SecretStore>,
        company: &CompanyId,
    ) -> crate::error::Result<Option<TenantChargebee>> {
        let read = async |key: &str| -> crate::error::Result<Option<String>> {
            Ok(secrets
                .get(company, key)
                .await?
                .map(|value| value.0.trim().to_string())
                .filter(|value| !value.is_empty()))
        };
        let (Some(site), Some(api_key)) = (read(SITE_SECRET).await?, read(API_KEY_SECRET).await?)
        else {
            return Ok(None);
        };
        Ok(Some(TenantChargebee {
            config: ChargebeeConfig { site, api_key },
        }))
    }

    /// The Chargebee site this company bills through. Never the key.
    pub fn site(&self) -> &str {
        &self.config.site
    }

    /// A stable hash of the connection, for the roster staleness check.
    ///
    /// Covers the site AND the key, so rotating a key with the site unchanged
    /// still rebuilds the roster — otherwise a rotated credential would keep
    /// authenticating with the old one until a restart.
    pub fn fingerprint(config: &Option<TenantChargebee>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match config {
            None => 0u8.hash(&mut hasher),
            Some(c) => {
                1u8.hash(&mut hasher);
                c.config.site.hash(&mut hasher);
                c.config.api_key.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

#[cfg(feature = "chargebee")]
pub use live::chargebee_tools;

#[cfg(feature = "chargebee")]
mod live {
    use super::*;

    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::{Value, json};

    use crate::chargebee::api;
    use crate::chargebee::client::ChargebeeClient;
    use crate::chargebee::types::{
        CreateCustomerArgs, GetCustomerArgs, GetInvoiceArgs, ListInvoicesArgs, SendInvoiceArgs,
    };

    use openhuman_core as oh;
    use tinytools::{PermissionLevel, Tool, ToolResult};

    /// Builds the five per-tenant Chargebee tools over a resolved connection.
    pub fn chargebee_tools(config: &TenantChargebee) -> Vec<Box<dyn Tool>> {
        let config = Arc::new(config.clone());
        vec![
            Box::new(SendInvoiceTool(Arc::clone(&config))),
            Box::new(GetInvoiceTool(Arc::clone(&config))),
            Box::new(ListInvoicesTool(Arc::clone(&config))),
            Box::new(GetCustomerTool(Arc::clone(&config))),
            Box::new(CreateCustomerTool(config)),
        ]
    }

    /// Builds the HTTP client for a call about to be made.
    ///
    /// Per call rather than once at construction so a key rotated mid-roster is
    /// never held open by a long-lived connection built from the old one.
    fn client(config: &TenantChargebee) -> crate::error::Result<ChargebeeClient> {
        ChargebeeClient::new(config.config.clone())
    }

    /// Renders a successful tool result, or the failure as text the agent can
    /// act on.
    ///
    /// A Chargebee rejection ("that currency is not enabled on this site") is a
    /// [`ToolResult::error`], not a transport failure: the call dispatched fine
    /// and produced an answer worth relaying. Collapsing the two would leave the
    /// agent unable to tell a broken integration from a business outcome.
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

    /// Parses tool arguments, reporting a bad shape as a tool error rather than
    /// failing the turn.
    fn parse<T: serde::de::DeserializeOwned>(args: Value) -> std::result::Result<T, ToolResult> {
        serde_json::from_value(args)
            .map_err(|e| ToolResult::error(format!("invalid arguments: {e}")))
    }

    /// The minor-unit warning, repeated in every schema that takes money. An
    /// agent reads one tool's schema, not the module docs.
    const MINOR_UNITS: &str = "Amount in the currency's MINOR unit. $100.00 USD is 10000, not 100.";

    pub struct SendInvoiceTool(Arc<TenantChargebee>);

    #[async_trait]
    impl Tool for SendInvoiceTool {
        fn name(&self) -> &str {
            "chargebee_send_invoice"
        }

        fn description(&self) -> &str {
            "Create and send a Chargebee invoice to a customer, identified by email. Creates the \
             customer automatically if no Chargebee record matches that email, so you never need \
             an internal customer id. Raises an UNPAID invoice — it does not charge a stored card \
             — and returns the invoice with a payment link when one can be raised."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "required": ["customer_email", "currency_code", "line_items"],
                "additionalProperties": false,
                "properties": {
                    "customer_email": {"type": "string", "description": "Who to invoice."},
                    "customer_name": {
                        "type": "string",
                        "description": "Only used if the customer has to be created; an existing customer is never renamed."
                    },
                    "currency_code": {"type": "string", "description": "ISO 4217, e.g. USD. Must be enabled on the Chargebee site."},
                    "line_items": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "required": ["description", "amount_in_minor_units"],
                            "additionalProperties": false,
                            "properties": {
                                "description": {"type": "string"},
                                "amount_in_minor_units": {"type": "integer", "minimum": 1, "description": MINOR_UNITS}
                            }
                        }
                    },
                    "due_days": {"type": "integer", "minimum": 0, "description": "Days until the invoice falls due."},
                    "invoice_note": {"type": "string"},
                    "idempotency_key": {
                        "type": "string",
                        "description": "Rarely needed. One is derived from the invoice automatically, so a retry of the same invoice cannot bill the customer twice. Supply a distinct value ONLY to raise a second, deliberately identical invoice for the same customer."
                    }
                }
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            // A real customer receives this. It parks.
            PermissionLevel::Execute
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let args: SendInvoiceArgs = match parse(args) {
                Ok(args) => args,
                Err(result) => return Ok(result),
            };
            let client = match client(&self.0) {
                Ok(client) => client,
                Err(e) => return Ok(ToolResult::error(format!("chargebee client: {e}"))),
            };
            // Deliberately no customer email: this line lands in durable host
            // logs, and a counterparty's address is their personal data, not
            // ours to retain for operational telemetry. The site and line count
            // are enough to trace a call.
            tracing::info!(
                site = %self.0.site(),
                line_items = args.line_items.len(),
                "[chargebee] send_invoice"
            );
            Ok(render(
                "chargebee_send_invoice",
                api::send_invoice(&client, args).await,
            ))
        }
    }

    pub struct GetInvoiceTool(Arc<TenantChargebee>);

    #[async_trait]
    impl Tool for GetInvoiceTool {
        fn name(&self) -> &str {
            "chargebee_get_invoice"
        }

        fn description(&self) -> &str {
            "Fetch one Chargebee invoice by id, with its current status (paid, payment_due, \
             voided), amount due and amount paid. Use this to answer whether an invoice has been \
             paid."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "required": ["invoice_id"],
                "additionalProperties": false,
                "properties": {"invoice_id": {"type": "string"}}
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let args: GetInvoiceArgs = match parse(args) {
                Ok(args) => args,
                Err(result) => return Ok(result),
            };
            let client = match client(&self.0) {
                Ok(client) => client,
                Err(e) => return Ok(ToolResult::error(format!("chargebee client: {e}"))),
            };
            Ok(render(
                "chargebee_get_invoice",
                api::get_invoice(&client, args).await,
            ))
        }
    }

    pub struct ListInvoicesTool(Arc<TenantChargebee>);

    #[async_trait]
    impl Tool for ListInvoicesTool {
        fn name(&self) -> &str {
            "chargebee_list_invoices"
        }

        fn description(&self) -> &str {
            "List Chargebee invoices, optionally narrowed to one customer by email and/or a \
             status. An email that matches no Chargebee customer returns an empty list, never the \
             whole site's invoices."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "customer_email": {"type": "string"},
                    "status": {
                        "type": "string",
                        "enum": ["paid", "posted", "payment_due", "not_paid", "voided", "pending"]
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100}
                }
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let args: ListInvoicesArgs = match parse(args) {
                Ok(args) => args,
                Err(result) => return Ok(result),
            };
            let client = match client(&self.0) {
                Ok(client) => client,
                Err(e) => return Ok(ToolResult::error(format!("chargebee client: {e}"))),
            };
            Ok(render(
                "chargebee_list_invoices",
                api::list_invoices(&client, args).await,
            ))
        }
    }

    pub struct GetCustomerTool(Arc<TenantChargebee>);

    #[async_trait]
    impl Tool for GetCustomerTool {
        fn name(&self) -> &str {
            "chargebee_get_customer"
        }

        fn description(&self) -> &str {
            "Look up a Chargebee customer by email. Returns nothing when no customer matches — \
             which is not an error. `chargebee_send_invoice` already creates a missing customer, \
             so you rarely need this first."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "required": ["email"],
                "additionalProperties": false,
                "properties": {"email": {"type": "string"}}
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let args: GetCustomerArgs = match parse(args) {
                Ok(args) => args,
                Err(result) => return Ok(result),
            };
            let client = match client(&self.0) {
                Ok(client) => client,
                Err(e) => return Ok(ToolResult::error(format!("chargebee client: {e}"))),
            };
            match api::get_customer(&client, &args.email).await {
                // "No such customer" is an answer, not a failure — an error here
                // would push the agent into apologising for a successful lookup.
                Ok(None) => Ok(ToolResult::success(format!(
                    "No Chargebee customer matches {}.",
                    args.email
                ))),
                other => Ok(render("chargebee_get_customer", other)),
            }
        }
    }

    pub struct CreateCustomerTool(Arc<TenantChargebee>);

    #[async_trait]
    impl Tool for CreateCustomerTool {
        fn name(&self) -> &str {
            "chargebee_create_customer"
        }

        fn description(&self) -> &str {
            "Create a Chargebee customer. Only needed to record someone ahead of invoicing them — \
             `chargebee_send_invoice` creates a missing customer on its own."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "required": ["email"],
                "additionalProperties": false,
                "properties": {
                    "email": {"type": "string"},
                    "name": {"type": "string", "description": "Full name; split into first/last."},
                    "company": {"type": "string"}
                }
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::Execute
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let args: CreateCustomerArgs = match parse(args) {
                Ok(args) => args,
                Err(result) => return Ok(result),
            };
            let client = match client(&self.0) {
                Ok(client) => client,
                Err(e) => return Ok(ToolResult::error(format!("chargebee client: {e}"))),
            };
            Ok(render(
                "chargebee_create_customer",
                api::create_customer(&client, args).await,
            ))
        }
    }
}

#[cfg(all(test, feature = "chargebee"))]
#[path = "chargebee_tests.rs"]
mod tests;
