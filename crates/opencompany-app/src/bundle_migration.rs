//! One-shot carry-over of state filed under the pre-rename bundle identifier.
//!
//! The desktop's identifier moved from `ai.tinyhumans.opencompany` to
//! `com.tinyhumans.opencompany`. The OS files some of an application's state
//! under that identifier, so on first launch the renamed build would otherwise
//! start from nothing:
//!
//! - **`~/Library/WebKit/<id>/`** (macOS) — the webview's `localStorage`, which
//!   is where the console keeps its saved connection profiles
//!   (`frontend/src/connections/profileStore.ts`). Losing it is the visible
//!   failure: every remote host the operator added disappears, and the
//!   `device-session:{connection}` keychain entries those profiles point at are
//!   stranded with it.
//! - **`~/Library/HTTPStorages/<id>/`** (macOS) — the webview's cookie store.
//! - **`~/Library/Application Support/<id>/`** (macOS), and the XDG / `%APPDATA%`
//!   equivalents elsewhere — Tauri's `app_*_dir`. Nothing in this crate writes
//!   there today (instance data lives under `~/.opencompany`, see
//!   [`default_data_dir`](crate::default_data_dir)), but on Windows and Linux
//!   the webview's own profile does, so it is carried over for the same reason.
//!
//! The keychain is NOT migrated: [`keychain`](crate::keychain) keeps the old
//! identifier as its service name on purpose, so its entries never move.
//!
//! This runs before the webview exists, which is why it resolves the
//! directories itself instead of through Tauri's path resolver (that needs an
//! `AppHandle`, and by the time one exists the window — and its fresh, empty
//! data store — has already been created).
//!
//! It COPIES rather than moves, so an old build still installed alongside keeps
//! working, and it never overwrites: a directory that already exists under the
//! new identifier is left alone, which is also what makes the step one-shot.
//! Failure is logged and never fatal — a desktop that will not start over a
//! stale cache is worse than one that starts empty.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The identifier every release before the rename shipped with.
pub const LEGACY_IDENTIFIER: &str = "ai.tinyhumans.opencompany";

/// The identifier in `tauri.conf.json`. A unit test holds the two equal.
pub const IDENTIFIER: &str = "com.tinyhumans.opencompany";

/// What [`migrate_dir`] did with one directory.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing was stored under the old identifier.
    NothingToMigrate,
    /// The new identifier already has its own directory; left untouched.
    AlreadyPresent,
    /// The old directory was copied to the new one.
    Copied,
}

/// The parent directories the OS files per-identifier state under, for `os`
/// (a [`std::env::consts::OS`] value), resolved from `env`.
///
/// Mirrors what Tauri's path resolver (via the `dirs` crate) and the platform
/// webviews use. A root whose variable is unset is skipped.
pub fn identifier_roots(os: &str, env: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let var = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    match os {
        "macos" => var("HOME")
            .map(|home| {
                let library = home.join("Library");
                vec![
                    library.join("Application Support"),
                    library.join("WebKit"),
                    library.join("HTTPStorages"),
                ]
            })
            .unwrap_or_default(),
        "windows" => ["APPDATA", "LOCALAPPDATA"]
            .iter()
            .filter_map(|name| var(name))
            .collect(),
        _ => {
            let home = var("HOME");
            let data = var("XDG_DATA_HOME")
                .or_else(|| home.as_ref().map(|h| h.join(".local").join("share")));
            let config =
                var("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config")));
            data.into_iter().chain(config).collect()
        }
    }
}

/// Copy `old` to `new` when `old` is a directory and `new` does not exist yet.
///
/// The copy lands in a sibling `<new>.migrating` first and is renamed into
/// place, so an interrupted run leaves no half-populated `new` that would make
/// the next launch think the migration already happened. A leftover staging
/// directory from such a run is discarded and the copy redone.
pub fn migrate_dir(old: &Path, new: &Path) -> io::Result<Outcome> {
    if new.symlink_metadata().is_ok() {
        return Ok(Outcome::AlreadyPresent);
    }
    if !old.is_dir() {
        return Ok(Outcome::NothingToMigrate);
    }
    let mut staging_name = new.file_name().map(OsString::from).unwrap_or_default();
    staging_name.push(".migrating");
    let staging = new.with_file_name(staging_name);
    if staging.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_tree(old, &staging)?;
    std::fs::rename(&staging, new)?;
    Ok(Outcome::Copied)
}

/// Recursive copy of regular files and directories. Symlinks, sockets and the
/// like are skipped: nothing a webview or Tauri stores needs them, and
/// following a link could copy something outside the tree.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Carry every legacy per-identifier directory over for this platform. Called
/// once from [`run`](crate::run), before the webview is created.
pub fn run() {
    let env = |name: &str| std::env::var_os(name);
    for root in identifier_roots(std::env::consts::OS, &env) {
        let old = root.join(LEGACY_IDENTIFIER);
        let new = root.join(IDENTIFIER);
        match migrate_dir(&old, &new) {
            Ok(Outcome::Copied) => tracing::info!(
                from = %old.display(),
                to = %new.display(),
                "carried desktop state over from the pre-rename bundle identifier"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                from = %old.display(),
                to = %new.display(),
                %error,
                "could not carry desktop state over from the pre-rename bundle identifier; starting without it"
            ),
        }
    }
}

#[cfg(test)]
#[path = "bundle_migration_tests.rs"]
mod tests;
