//! The `#[tauri::command]` surface the console calls.
//!
//! Thin by design: every one of these delegates to [`crate::proxy`] or
//! [`crate::embedded`], which are plain Rust and testable without a webview.
//! Logic that lives in a command is logic that can only be exercised by
//! starting a GUI.
//!
//! **Every command takes an explicit `connection_id`.** None of them reads an
//! "active connection" from application state — that single-valued field is
//! exactly what stops block/buzz from holding more than one workspace at a
//! time, and a command that defaulted it would reintroduce the limit invisibly.

use tauri::State;
use tauri::ipc::Channel;

use crate::local::LocalInstanceInfo;
use crate::proxy::{
    Connection, Credential, ProxyRequest, ProxyResponse, SharedProxy, may_carry_a_credential,
};

/// What the console needs to construct a connection record.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedInfo {
    pub base_url: String,
    pub data_dir: String,
    /// Who is answering there, as opposed to where.
    ///
    /// Carried because `base_url` holds an ephemeral port and so cannot be an
    /// identity: keyed on the address, the console reads every launch as a
    /// first meeting and leaves the previous launch's row behind, dead (#615).
    pub instance_id: String,
    /// The address this host admits, for the sign-in form to offer (#632).
    ///
    /// A desktop install has no mail transport and one standing admin, so the
    /// person in front of it cannot discover what to type and the host will
    /// answer every other address with the same silent 202. Carrying it is what
    /// turns the login form from a guessing game into a click.
    ///
    /// Not a credential: it names who may sign in, and the code that actually
    /// does it is minted per attempt and returned only on a loopback host.
    pub operator_email: String,
}

/// Registers (or re-registers) a host this client talks to.
///
/// **Takes no device token.** The console cannot supply one, because it has
/// never seen one: a paired device's session is resolved from the keychain by
/// `connection_id`. That is the difference between "the webview does not
/// normally hold the secret" and "the webview cannot hold the secret", and only
/// the second survives a script injected into rendered agent markdown.
#[tauri::command]
pub async fn oc_connect(
    proxy: State<'_, SharedProxy>,
    connection_id: String,
    base_url: String,
    platform_token: Option<String>,
) -> Result<(), String> {
    // Device first: a paired device is a *person* on this machine, and the
    // journal records their name. A platform bearer is a machine credential
    // that writes anonymously, so preferring it would silently un-attribute
    // every write the desktop makes.
    let credential = match (
        crate::keychain::device_session(&connection_id),
        platform_token,
    ) {
        (Some(session), _) => Credential::Device(session),
        (None, Some(token)) => Credential::Platform(token),
        (None, None) => Credential::None,
    };
    proxy
        .upsert(
            connection_id,
            Connection {
                base_url,
                credential,
            },
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn oc_disconnect(
    proxy: State<'_, SharedProxy>,
    connection_id: String,
) -> Result<(), String> {
    proxy.remove(&connection_id).await;
    Ok(())
}

#[tauri::command]
pub async fn oc_connections(proxy: State<'_, SharedProxy>) -> Result<Vec<String>, String> {
    Ok(proxy.ids().await)
}

/// One HTTP request against a named connection.
#[tauri::command]
pub async fn oc_request(
    proxy: State<'_, SharedProxy>,
    connection_id: String,
    request: ProxyRequest,
) -> Result<ProxyResponse, String> {
    proxy
        .request(&connection_id, request)
        .await
        .map_err(|error| error.to_string())
}

/// Subscribes to a connection's event stream, pushing payloads down `channel`.
///
/// One channel per subscription rather than one shared bus: a chatty company's
/// turn events must not be able to starve another connection's, and dropping
/// the channel is how the console unsubscribes.
#[tauri::command]
pub async fn oc_subscribe(
    proxy: State<'_, SharedProxy>,
    connection_id: String,
    path: String,
    channel: Channel<String>,
) -> Result<(), String> {
    let proxy = proxy.inner().clone();
    tokio::spawn(async move {
        let result = proxy
            .subscribe(&connection_id, &path, |event| {
                // A send failure means the console dropped the channel, i.e.
                // unsubscribed. Not an error worth reporting.
                let _ = channel.send(event);
            })
            .await;
        if let Err(error) = result {
            tracing::debug!(%error, "event stream ended");
        }
    });
    Ok(())
}

/// What the console learns after pairing. Carries no secret.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub company: String,
    pub device_id: String,
    pub expires_at_millis: u64,
}

/// What the host answers a claim with. The token half never leaves this module.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaimedDevice {
    token: String,
    company: String,
    device_id: String,
    expires_at_millis: u64,
}

/// Redeems a pairing code against a host, and returns what it answered.
///
/// Split out of [`oc_pair_device`] so it can be tested at all: a command takes
/// `State<'_, SharedProxy>`, which needs a Tauri application to construct, and
/// the module note above says why that matters — logic reachable only by
/// starting a GUI is logic nothing checks. Everything here is the part with a
/// rule in it, and the command below is the keychain write and the
/// re-registration around it.
///
/// **The one exchange in which a session token is created rather than
/// replayed.** The pairing code goes out in the request and the token comes
/// back in the response body, so this is where an unencrypted wire costs the
/// most — and it is not covered by `ProxyRegistry::upsert`, because it never
/// goes through the registry (#731).
async fn claim(base_url: &str, code: &str, label: Option<&str>) -> Result<ClaimedDevice, String> {
    if !may_carry_a_credential(base_url) {
        // The console shows this verbatim (`device-pairing.tsx`), so it is
        // written for the person reading it rather than for a log.
        return Err(format!(
            "{base_url} is not encrypted, so pairing would send this device's session in the clear. Use https, or a host on this machine."
        ));
    }
    let base = base_url.trim_end_matches('/');
    let response = reqwest::Client::builder()
        // As `ProxyRegistry`'s client does, and here for a sharper reason: the
        // default policy follows up to ten redirects, and a 307 from an https
        // base to an http one re-sends this request — pairing code and all —
        // over the wire the check above just refused. A check on the first url
        // is worth nothing if the client will walk to a second.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?
        .post(format!("{base}/api/v1/devices/claim"))
        .json(&serde_json::json!({ "code": code, "label": label }))
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        // The host's own wording, which is deliberately one indistinguishable
        // message for every way a claim can fail. Passing it through keeps that
        // property instead of inventing a more specific one here.
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("pairing failed with {status}"));
        return Err(message);
    }

    response.json().await.map_err(|error| error.to_string())
}

/// Redeems a pairing code, keeping the session token out of the webview.
///
/// The whole flow lives in Rust for one reason: the token exists for exactly
/// one HTTP response, and the console must not be on the path it takes. So this
/// command performs the claim, writes the result to the keychain, re-registers
/// the connection with the resolved credential, and returns only what a person
/// needs to see — which company, which device, how long it lasts.
///
/// Deliberately does its own request rather than going through
/// `ProxyRegistry::request`: this runs *before* the connection has a credential
/// worth attaching, and routing it through the proxy would mean a code path
/// where the claim response body — the one place a raw token appears — passes
/// through the same machinery that serialises bodies back to the webview.
///
/// Which is also why the transport rule has to be repeated here. Doing its own
/// request means doing its own checking: the registry never sees this url, so
/// `upsert`'s refusal does not cover the one exchange where the token is not
/// merely replayed but *handed over* — the code goes out in the request and the
/// session comes back in the response, both in the clear on a plain-HTTP host.
#[tauri::command]
pub async fn oc_pair_device(
    proxy: State<'_, SharedProxy>,
    connection_id: String,
    base_url: String,
    code: String,
    label: Option<String>,
) -> Result<PairedDevice, String> {
    let claimed = claim(&base_url, &code, label.as_deref()).await?;
    // `<company>.<token>` is the header carrier's form, and the only form
    // anything downstream needs.
    crate::keychain::remember_device(
        &connection_id,
        &format!("{}.{}", claimed.company, claimed.token),
    )
    .map_err(|error| error.to_string())?;

    // Re-register so the credential takes effect without waiting for a reload.
    // The console cannot do this itself — it has nothing to pass.
    //
    // The session is read back from the keychain rather than reused from the
    // claim: what matters is what the store will hand out on the *next* boot,
    // so a write that did not survive surfaces here rather than as a mysterious
    // 401 later. A miss is `Credential::None`, never `Device("")` — an empty
    // session header is a credential that authenticates as nobody while looking
    // like one to every check that only asks whether a device is paired.
    if let Ok(base_url) = proxy.base_url(&connection_id).await {
        let credential = match crate::keychain::device_session(&connection_id) {
            Some(session) => Credential::Device(session),
            None => Credential::None,
        };
        // Infallible in practice: `base_url` was just read back out of the
        // registry, so it is one `upsert` already accepted. Surfaced rather
        // than swallowed anyway — a pairing that reported success while the
        // credential never took effect is the worst of the three outcomes.
        proxy
            .upsert(
                connection_id.clone(),
                Connection {
                    base_url,
                    credential,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
    }

    Ok(PairedDevice {
        company: claimed.company,
        device_id: claimed.device_id,
        expires_at_millis: claimed.expires_at_millis,
    })
}

/// Forgets this machine's stored session for a connection.
///
/// Local only. The session record on the host outlives it — revoking that is
/// the operator's action from the devices list, and doing both here would mean
/// removing a row from one machine silently cut off another.
#[tauri::command]
pub async fn oc_forget_device(
    proxy: State<'_, SharedProxy>,
    connection_id: String,
) -> Result<(), String> {
    crate::keychain::forget_device(&connection_id).map_err(|error| error.to_string())?;
    if let Ok(base_url) = proxy.base_url(&connection_id).await {
        // As in `oc_pair_device`: a url the registry already accepted.
        proxy
            .upsert(
                connection_id,
                Connection {
                    base_url,
                    credential: Credential::None,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Where the host rooted at the data dir is listening, if it is running.
///
/// Kept alongside [`oc_local_instances`], which supersedes it, because the two
/// halves of this application ship independently: a `pnpm dev` console built
/// before the roster existed calls only this, and a shell built before it
/// answers only this. Both degrade to the single-instance behaviour instead of
/// to an unhandled `no such command`.
#[tauri::command]
pub async fn oc_embedded(
    state: State<'_, crate::AppHandleState>,
) -> Result<Option<EmbeddedInfo>, String> {
    let local = state.local.lock().await;
    Ok(local.default_instance().and_then(|instance| {
        Some(EmbeddedInfo {
            base_url: instance.base_url?,
            data_dir: instance.data_dir,
            instance_id: instance.instance_id?,
            operator_email: instance.operator_email?,
        })
    }))
}

/// Every host this machine runs, listening or not.
///
/// The listing is the whole surface: creating, starting and stopping all
/// answer with the affected instance, and the console re-reads this rather
/// than keeping its own idea of the roster. One source of truth, on the side
/// that actually holds the sockets.
#[tauri::command]
pub async fn oc_local_instances(
    state: State<'_, crate::AppHandleState>,
) -> Result<Vec<LocalInstanceInfo>, String> {
    Ok(state.local.lock().await.list())
}

/// Adds a host over a fresh data root on this machine, and starts it.
///
/// Its own root, never a second process over an existing one: two hosts over
/// one root overwrite each other's companies, which is why `prepare_instance`
/// locks it in the first place.
#[tauri::command]
pub async fn oc_create_local_instance(
    state: State<'_, crate::AppHandleState>,
    label: String,
) -> Result<LocalInstanceInfo, String> {
    state.local.lock().await.create(&label).await
}

#[tauri::command]
pub async fn oc_start_local_instance(
    state: State<'_, crate::AppHandleState>,
    id: String,
) -> Result<LocalInstanceInfo, String> {
    state.local.lock().await.start(&id).await
}

/// Stops a host, freeing its port and — the part that matters — its data root,
/// so a terminal `opencompany serve` can take it.
#[tauri::command]
pub async fn oc_stop_local_instance(
    state: State<'_, crate::AppHandleState>,
    id: String,
) -> Result<LocalInstanceInfo, String> {
    state.local.lock().await.stop(&id)
}

#[tauri::command]
pub async fn oc_rename_local_instance(
    state: State<'_, crate::AppHandleState>,
    id: String,
    label: String,
) -> Result<LocalInstanceInfo, String> {
    state.local.lock().await.rename(&id, &label)
}

/// Drops a host from the roster. **Leaves its data on disk** — see
/// [`crate::local::LocalHosts::forget`].
#[tauri::command]
pub async fn oc_forget_local_instance(
    state: State<'_, crate::AppHandleState>,
    id: String,
) -> Result<(), String> {
    let mut local = state.local.lock().await;
    // Stopping first is what makes the removal complete: a forgotten instance
    // whose host kept listening would hold its root against the terminal, and
    // stay reachable from a console row nothing lists any more.
    let _ = local.stop(&id);
    local.forget(&id)
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// The guarantee the console cannot make about itself.
    ///
    /// `pairDevice` in the console returns whatever the core sends, so a mock
    /// there proves nothing — this is where "the token never reaches the
    /// webview" is actually enforced, by a type that has nowhere to put one.
    /// If a `token` field is ever added to `PairedDevice`, this fails.
    #[test]
    fn a_paired_device_carries_no_token() {
        let wire = serde_json::to_value(PairedDevice {
            company: "acme".into(),
            device_id: "dev-1".into(),
            expires_at_millis: 1,
        })
        .expect("serialise");

        // Sorted, for the same reason as the instance row below: the closed set
        // is the claim. `PairedDevice`'s field order happens to be alphabetical
        // today, so an ordered comparison passes by coincidence rather than by
        // design — and would go red the moment a field is inserted out of that
        // position, for a reason that has nothing to do with what this test is
        // about.
        let mut keys: Vec<&str> = wire
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["company", "deviceId", "expiresAtMillis"],
            "pairing must answer with these three fields and nothing else"
        );
        assert!(!wire.to_string().to_lowercase().contains("token"));
    }

    /// The keys the console reads off an instance row, by name.
    ///
    /// Same argument as `the_embedded_record_answers_in_the_keys_the_console_reads`:
    /// nothing type-checks a Rust struct against the TypeScript that reads it,
    /// and every optional field degrades silently. A renamed key lands as "the
    /// instance list is full of blank rows", not as an error.
    #[test]
    fn an_instance_row_answers_in_the_keys_the_console_reads() {
        let wire = serde_json::to_value(LocalInstanceInfo {
            id: "acme".into(),
            label: "Acme".into(),
            data_dir: "/data/instances/acme".into(),
            running: true,
            base_url: Some("http://127.0.0.1:1234".into()),
            instance_id: Some("inst-1".into()),
            operator_email: Some("operator@opencompany.local".into()),
            companies: vec!["acme".into()],
            error: None,
        })
        .expect("serialise");

        // Sorted before comparing, because the set is what this asserts and the
        // order is not. This crate inherits `serde_json`'s `preserve_order`
        // through its path dependency on `opencompany` (root `Cargo.toml:86`),
        // so a JSON object is backed by an `IndexMap` and emits **struct field
        // order**, not alphabetical order. Pinning the order here asserted a
        // property nothing needs — JSON object order means nothing to the
        // TypeScript that reads these by name — and it would break again the
        // next time a field is added in the middle of the struct.
        let mut keys: Vec<&str> = wire
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "baseUrl",
                "companies",
                "dataDir",
                "id",
                "instanceId",
                "label",
                "operatorEmail",
                "running",
            ],
            "the instance row answers in exactly these keys: {wire}"
        );
    }

    /// A stopped row carries no address, so the console cannot render one that
    /// would fail its probe forever.
    #[test]
    fn a_stopped_instance_carries_no_address() {
        let wire = serde_json::to_value(LocalInstanceInfo {
            id: "acme".into(),
            label: "Acme".into(),
            data_dir: "/data/instances/acme".into(),
            running: false,
            base_url: None,
            instance_id: None,
            operator_email: None,
            companies: Vec::new(),
            error: Some("the data root is in use".into()),
        })
        .expect("serialise");

        let object = wire.as_object().expect("an object");
        assert!(!object.contains_key("baseUrl"));
        assert_eq!(object["error"], "the data root is in use");
        assert_eq!(object["running"], false);
    }

    /// A one-shot host that answers every request with `head`, then closes.
    ///
    /// Returns its base url and a handle that says whether anything ever
    /// connected — which is the assertion for a refusal that must happen
    /// *before* the wire, not on the answer that comes back over it.
    async fn host(head: &'static str) -> (String, Arc<AtomicBool>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let reached = Arc::new(AtomicBool::new(false));
        let flag = reached.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                flag.store(true, Ordering::SeqCst);
                use tokio::io::AsyncWriteExt as _;
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{address}"), reached)
    }

    /// The claim is refused before a socket is opened, not after an answer.
    ///
    /// A token that travelled once has been read; there is no recovering from
    /// it by rejecting the response. So this asserts on the connection, not on
    /// the `Err` — the message alone would pass on a version that sent the
    /// pairing code first and complained afterwards (#731).
    #[tokio::test]
    async fn pairing_over_an_unencrypted_remote_host_sends_nothing() {
        // A real listener, addressed by a name that is not loopback. The
        // resolver never runs, because the refusal comes first — which is the
        // point.
        let (_, reached) = host("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
        for base in [
            "http://192.168.1.20:8080",
            "http://acme.example.com",
            "http://10.0.0.4:8080",
        ] {
            // `let else` rather than `expect_err`, which would need
            // `ClaimedDevice: Debug` — and a `token` field behind a `{:?}` is
            // the thing the type is shaped to prevent.
            let Err(error) = claim(base, "code-123", None).await else {
                panic!("{base} must not be paired with");
            };
            assert!(
                error.contains("not encrypted"),
                "{base} must be refused for the reason it is refused: {error}"
            );
        }
        assert!(
            !reached.load(Ordering::SeqCst),
            "nothing may be sent to a host the rule refuses"
        );
    }

    /// Loopback still pairs — the embedded host is reached no other way.
    #[tokio::test]
    async fn pairing_with_a_host_on_this_machine_still_works() {
        let body = r#"{"token":"t","company":"acme","deviceId":"dev-1","expiresAtMillis":1}"#;
        let head: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let (base, reached) = host(head).await;

        let claimed = claim(&base, "code-123", Some("a laptop"))
            .await
            .expect("a loopback host pairs");

        assert!(reached.load(Ordering::SeqCst));
        assert_eq!(claimed.company, "acme");
        assert_eq!(claimed.device_id, "dev-1");
    }

    /// A redirect is not followed, so an https base cannot be walked to http.
    ///
    /// `reqwest`'s default policy follows up to ten, and a 307 re-sends the
    /// body — so a host answering `307 → http://…` would put the pairing code
    /// on exactly the wire the check above refuses, having passed it. Checking
    /// the first url is worth nothing if the client will walk to a second.
    #[tokio::test]
    async fn a_redirect_away_from_the_checked_host_is_not_followed() {
        let (base, _) = host(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://192.168.1.20:8080/api/v1/devices/claim\r\ncontent-length: 0\r\n\r\n",
        )
        .await;

        let Err(error) = claim(&base, "code-123", None).await else {
            panic!("a redirect is an answer, not a detour to follow");
        };
        // The host's status, passed through — which is what "not followed"
        // looks like from here.
        assert!(error.contains("307"), "{error}");
    }

    /// The console reads these keys by name, and a rename here is silent on
    /// both sides: TypeScript has nothing to check a Rust struct against, and
    /// every field is optional in the console precisely so an older shell
    /// degrades instead of failing. A wrong key would therefore land as "the
    /// sign-in form is blank again" (#632) rather than as an error.
    #[test]
    fn the_embedded_record_answers_in_the_keys_the_console_reads() {
        let wire = serde_json::to_value(EmbeddedInfo {
            base_url: "http://127.0.0.1:1234".into(),
            data_dir: "/data".into(),
            instance_id: "inst-1".into(),
            operator_email: "operator@opencompany.local".into(),
        })
        .expect("serialise");

        // Sorted, as above. `EmbeddedInfo`'s field order is simultaneously
        // struct order and alphabetical, which is why this test passed either
        // way and why it could never have established the precedent the
        // instance-row test cited it for.
        let mut keys: Vec<&str> = wire
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["baseUrl", "dataDir", "instanceId", "operatorEmail"],
            "the embedded record answers in exactly these keys: {wire}"
        );
    }
}
