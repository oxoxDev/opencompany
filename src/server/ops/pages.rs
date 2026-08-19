//! Serves agent-authored internal dashboard pages (`Pages/<slug>/` in the
//! company workspace) to the operator console.
//!
//! ```text
//! GET …/pages                    every page's manifest, for the nav
//! GET …/pages/{slug}             a fixed HTML shell that mounts the page
//! GET …/pages/{slug}/bundle.mjs  the page's compiled JS, streamed
//! ```
//!
//! See `docs/spec/runtime/pages.md` for the full design. The load-bearing
//! difference from `…/workspace/blob/{id}` ([`super::workspace::read_blob`]),
//! which *refuses* to render anything inline for exactly this class of risk
//! (issue #667): the bytes served here are never a raw upload. They are the
//! output of [`crate::harness::pages_tools::compile_page`] — a TSX source
//! parsed, import-checked against an allow-list, and re-rendered by `swc_core`
//! — so serving them as `application/javascript` with `Content-Disposition:
//! inline` is serving *validated compiled output*, not an arbitrary payload a
//! caller uploaded. The isolation boundary a browser actually needs — this is
//! still third-party-authored code running in the browser — is the sandboxed
//! iframe (`sandbox="allow-scripts"`, no `allow-same-origin`) the frontend
//! embeds it in, and the CSP headers below are defense in depth on top of
//! that, not a substitute for it.
//!
//! `harness::pages_tools` is compiled only under the `openhuman` feature (all
//! of `src/harness/` is); this module is always compiled, because the routes
//! it serves must 404 rather than fall through to the console SPA shell even
//! in a build without the harness. So it does not import from
//! `harness::pages_tools` — it re-derives the same `Pages/<slug>/` layout from
//! the always-compiled constants in
//! [`crate::company::workspace_scaffold`], the same way `harness::pages_tools`
//! does.

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::company::workspace_scaffold::{
    PAGE_COMPILED_MIME, PAGE_COMPILED_NAME, PAGE_MANIFEST_NAME, PAGES_ROOT,
};
use crate::error::OpenCompanyError;
use crate::ports::types::CompanyId;
use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceStore};
use crate::server::error::ApiError;
use crate::server::ops::{ScopedCompany, scoped};

/// The Content-Security-Policy every route in this module sets (plan §5).
///
/// `script-src 'self'` plus `'unsafe-inline'` covers the fixed HTML shell's
/// inline `<script type="module">`; `connect-src 'none'` means the shell
/// itself cannot open its own network requests — the page's real data access
/// is the frontend's postMessage bridge to the parent console tab, which this
/// header does not need to permit because it never leaves the frame as a
/// request this origin makes. `frame-ancestors 'self'` keeps the shell from
/// being embedded anywhere but this console.
const PAGES_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
     connect-src 'none'; frame-ancestors 'self'";

/// Builds the pages route fragment.
pub fn router() -> Router<AppState> {
    scoped("/pages", get(list_pages))
        .merge(scoped("/pages/{slug}", get(page_shell)))
        .merge(scoped("/pages/{slug}/bundle.mjs", get(bundle)))
}

#[derive(Debug, serde::Deserialize)]
struct SlugPath {
    slug: String,
}

/// One page's manifest, as the console nav consumes it.
///
/// Field names match what `PagesView.tsx` (the frontend nav, built alongside
/// this route) reads: `slug`, `title`, `description`, `icon`, `navVisible`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageListing {
    slug: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    nav_visible: bool,
}

/// A page's small manifest, as stored in `page.toml`.
///
/// Mirrors `harness::pages_tools::PageManifest` field-for-field; kept as a
/// separate type rather than shared because that one lives behind the
/// `openhuman` feature and this route must parse the same TOML without it.
#[derive(Debug, Deserialize)]
struct StoredManifest {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default = "default_nav_visible")]
    nav_visible: bool,
}

fn default_nav_visible() -> bool {
    true
}

/// A slug's resolved bundle: whichever of its three nodes exist.
struct PageBundle {
    manifest: Option<WorkspaceNode>,
    compiled: Option<WorkspaceNode>,
}

/// Whether `slug` is a safe path segment to build a workspace lookup and a
/// URL path from.
///
/// The HTML this route serves is a fixed Rust format string — not agent
/// content — so there is no injection risk from the slug reaching the
/// response body; this check exists so a malformed slug resolves to a clean
/// 404 instead of an ambiguous or surprising tree lookup. Mirrors
/// `harness::pages_tools::valid_slug`.
fn valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Resolves every `Pages/<slug>/` bundle from one company-scoped tree read.
async fn all_pages(
    store: &dyn WorkspaceStore,
    company: &CompanyId,
) -> crate::Result<Vec<(String, PageBundle)>> {
    let nodes = store.tree(company).await?;
    let Some(pages_root) = nodes
        .iter()
        .find(|n| n.parent_id.is_none() && n.kind == NodeKind::Folder && n.name == PAGES_ROOT)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for folder in nodes.iter().filter(|n| {
        n.kind == NodeKind::Folder && n.parent_id.as_deref() == Some(pages_root.id.as_str())
    }) {
        let mut bundle = PageBundle {
            manifest: None,
            compiled: None,
        };
        for child in nodes
            .iter()
            .filter(|n| n.parent_id.as_deref() == Some(folder.id.as_str()))
        {
            if child.name == PAGE_MANIFEST_NAME {
                bundle.manifest = Some(child.clone());
            } else if child.name == PAGE_COMPILED_NAME {
                bundle.compiled = Some(child.clone());
            }
        }
        out.push((folder.name.clone(), bundle));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

async fn read_manifest(
    store: &dyn WorkspaceStore,
    company: &CompanyId,
    node: &WorkspaceNode,
    fallback_title: &str,
) -> StoredManifest {
    let body = match store.read(company, &node.id).await {
        Ok(Some((_, body))) => body,
        _ => String::new(),
    };
    toml::from_str(&body).unwrap_or_else(|_| StoredManifest {
        title: fallback_title.to_string(),
        description: None,
        icon: None,
        nav_visible: true,
    })
}

/// `GET {scope}/pages` — every page's manifest, for the console nav.
async fn list_pages(company: ScopedCompany) -> Result<Response, ApiError> {
    let pages = all_pages(company.runtime.workspace().as_ref(), company.id()).await?;
    let mut listings = Vec::with_capacity(pages.len());
    for (slug, bundle) in &pages {
        let manifest = match &bundle.manifest {
            Some(node) => {
                read_manifest(
                    company.runtime.workspace().as_ref(),
                    company.id(),
                    node,
                    slug,
                )
                .await
            }
            None => StoredManifest {
                title: slug.clone(),
                description: None,
                icon: None,
                nav_visible: true,
            },
        };
        listings.push(PageListing {
            slug: slug.clone(),
            title: manifest.title,
            description: manifest.description,
            icon: manifest.icon,
            nav_visible: manifest.nav_visible,
        });
    }
    let mut response = Json(listings).into_response();
    apply_pages_headers(response.headers_mut());
    Ok(response)
}

/// The fixed HTML shell that mounts a page's compiled module (not agent
/// content). Extracted from the route so the shell's load-bearing invariants
/// — the React namespace import, the slug-relative bundle path, the absolute
/// SDK CSS link, the import map — are unit-testable instead of living only in
/// a route that needs a full workspace to exercise.
fn page_shell_html(slug: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{slug}</title>
<link rel="stylesheet" href="/pages-sdk/index.css">
<script type="importmap">
{{
  "imports": {{
    "react": "/pages-sdk/react.mjs",
    "react-dom/client": "/pages-sdk/react.mjs",
    "react/jsx-runtime": "/pages-sdk/react.mjs",
    "@opencompany/site": "/pages-sdk/index.mjs"
  }}
}}
</script>
</head>
<body>
<div id="root"></div>
<script type="module">
  import * as React from "react";
  import * as ReactDOM from "react-dom/client";
  import Page from "./{slug}/bundle.mjs";
  const root = ReactDOM.createRoot(document.getElementById("root"));
  root.render(React.createElement(Page));
</script>
</body>
</html>
"#,
        slug = slug,
    )
}

/// `GET {scope}/pages/{slug}` — a fixed HTML shell that mounts the page.
///
/// Not agent content: the slug is validated and interpolated into a literal
/// Rust format string, so nothing the page's own source contains ever reaches
/// this response.
async fn page_shell(
    company: ScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
) -> Result<Response, ApiError> {
    if !valid_slug(&slug) {
        return Err(ApiError(OpenCompanyError::NotFound(format!("page {slug}"))));
    }
    let pages = all_pages(company.runtime.workspace().as_ref(), company.id()).await?;
    if !pages.iter().any(|(name, _)| name == &slug) {
        return Err(ApiError(OpenCompanyError::NotFound(format!("page {slug}"))));
    }

    let html = page_shell_html(&slug);

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .map_err(|e| ApiError(OpenCompanyError::Store(format!("page shell failed: {e}"))))?;
    apply_pages_headers(response.headers_mut());
    Ok(response)
}

/// `GET {scope}/pages/{slug}/bundle.mjs` — the page's compiled JS, streamed.
async fn bundle(
    company: ScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
) -> Result<Response, ApiError> {
    if !valid_slug(&slug) {
        return Err(ApiError(OpenCompanyError::NotFound(format!("page {slug}"))));
    }
    let pages = all_pages(company.runtime.workspace().as_ref(), company.id()).await?;
    let Some((_, bundle)) = pages.into_iter().find(|(name, _)| name == &slug) else {
        return Err(ApiError(OpenCompanyError::NotFound(format!("page {slug}"))));
    };
    let Some(compiled) = bundle.compiled else {
        return Err(ApiError(OpenCompanyError::NotFound(format!(
            "page {slug} has not been compiled yet"
        ))));
    };

    let Some((node, stream)) = company
        .runtime
        .workspace()
        .read_bytes(company.id(), &compiled.id)
        .await?
    else {
        return Err(ApiError(OpenCompanyError::NotFound(format!(
            "page {slug} bundle"
        ))));
    };

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, PAGE_COMPILED_MIME)
        .header(header::CONTENT_DISPOSITION, "inline")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    if let Some(sha) = &node.sha256 {
        response = response.header(header::ETAG, format!("\"{sha}\""));
    }
    if let Some(size) = node.size {
        response = response.header(header::CONTENT_LENGTH, size);
    }
    let mut response = response.body(Body::from_stream(stream)).map_err(|e| {
        ApiError(OpenCompanyError::Store(format!(
            "bundle response failed: {e}"
        )))
    })?;
    apply_pages_headers(response.headers_mut());
    Ok(response)
}

/// Sets the CSP and `X-Content-Type-Options` headers every route in this
/// module carries (plan §5), without disturbing whatever content-type header
/// the caller already set.
fn apply_pages_headers(headers: &mut axum::http::HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGES_CSP),
    );
    // Authenticated, company-specific content: never let a browser or an
    // intermediary cache reuse another company's (or another session's) page
    // shell, manifest, or bundle.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation_matches_the_tool_side() {
        assert!(valid_slug("revenue"));
        assert!(valid_slug("revenue-2"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("Revenue"));
        assert!(!valid_slug("../secrets"));
        assert!(!valid_slug("rev enue"));
    }

    #[test]
    fn shell_imports_the_react_namespace_before_using_create_element() {
        let html = page_shell_html("revenue");
        // Bug this guards (PR #985): the shell called `React.createElement`
        // without ever importing `React`, so every page threw a ReferenceError
        // on first render.
        let module_script = html
            .split("<script type=\"module\">")
            .nth(1)
            .expect("the shell has a module script");
        let react_import = module_script.lines().find(|l| l.contains("React"));
        assert!(
            react_import.is_some(),
            "no `React` import in: {module_script}"
        );
    }

    #[test]
    fn shell_bundle_path_is_relative_to_the_shells_own_url() {
        let html = page_shell_html("revenue");
        // Bug this guards (PR #985): the shell imported `./bundle.mjs`, which
        // resolves against `…/pages/{slug}` (no trailing slash) to
        // `…/pages/bundle.mjs` — the shell route with slug "bundle.mjs", which
        // fails `valid_slug` and 404s. `./{slug}/bundle.mjs` resolves to the
        // registered bundle route.
        let module_script = html
            .split("<script type=\"module\">")
            .nth(1)
            .expect("the shell has a module script");
        assert!(
            module_script.contains("from \"./revenue/bundle.mjs\""),
            "bundle import must name the slug explicitly: {module_script}"
        );
        assert!(
            !module_script.contains("from \"./bundle.mjs\""),
            "the bare `./bundle.mjs` form must not return: {module_script}"
        );
    }

    #[test]
    fn shell_links_the_sdk_css_and_maps_react_jsx_runtime_to_the_sdk_bundle() {
        let html = page_shell_html("revenue");
        // Bug this guards (PR #985): the SDK's `index.css` was built and
        // shipped but never linked, so every page rendered unstyled.
        assert!(
            html.contains("<link rel=\"stylesheet\" href=\"/pages-sdk/index.css\">"),
            "the SDK stylesheet must be linked in the shell"
        );
        // The import map is what lets the compiler's automatic-jsx output
        // (`import { jsx } from "react/jsx-runtime"`) link at all.
        assert!(
            html.contains("\"react/jsx-runtime\": \"/pages-sdk/react.mjs\""),
            "react/jsx-runtime must resolve to the SDK's React bundle"
        );
    }
}
