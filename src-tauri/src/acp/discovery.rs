//! Finding the coding harnesses installed on this machine, and whether they can
//! actually run.
//!
//! ## Four states, not two
//!
//! The tempting model is available / unavailable. It is wrong, and wrong in a
//! way that costs an operator real time: **installed but not signed in** is the
//! single most common state on a fresh machine, and it looks identical to "not
//! installed" if all you check is `which`. The fix is completely different —
//! `claude login` versus installing anything — so collapsing them means the app
//! tells someone to do the wrong thing.
//!
//! [`Readiness`] therefore distinguishes:
//!
//! - `NotInstalled` — no binary on `PATH`. *Install it.*
//! - `NotSignedIn` — a binary that runs, with no credential. *Sign in.*
//! - `Ready` — spawnable and authenticated.
//! - `SpawnFailed` — present but refused to start. *Read the reason.*
//!
//! ## Why sign-in is probed by file and not by running the harness
//!
//! Asking a harness whether it is logged in means starting it, which is slow
//! (hundreds of milliseconds each, on a list refreshed whenever a settings pane
//! opens) and, for some, prompts interactively. Each harness instead keeps its
//! credential in a known place, so presence of that file is the probe. It can
//! be wrong in one direction — a stale or expired credential reads as signed in
//! — and that is the acceptable direction: the failure surfaces on first use
//! with the harness's own message, which is more accurate than anything guessed
//! here.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Whether a harness can be used right now, and if not, what to do about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Readiness {
    NotInstalled,
    NotSignedIn,
    Ready,
    SpawnFailed { reason: String },
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Readiness::Ready)
    }
}

/// One harness this client knows how to drive over ACP.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Harness {
    /// Stable id the console and the connection records use.
    pub id: &'static str,
    pub label: &'static str,
    /// The executable, looked up on `PATH`.
    pub command: &'static str,
    /// Arguments that put it into ACP mode.
    pub args: &'static [&'static str],
    /// Where it keeps its credential, relative to the user's home.
    ///
    /// `None` for a harness with no separate sign-in — one configured purely by
    /// environment variables, say — which reads as ready once it is installed.
    pub credential: Option<&'static str>,
}

/// The harnesses the desktop can drive.
///
/// A fixed list rather than discovery-by-convention: each entry encodes how to
/// put *that* harness into ACP mode, and guessing those arguments wrong spawns
/// a process that hangs waiting for interactive input.
pub const HARNESSES: &[Harness] = &[
    Harness {
        id: "claude",
        label: "Claude Code",
        // Confirmed live (issue #1245): `npm install -g
        // @agentclientprotocol/claude-agent-acp` installs a binary named
        // `claude-agent-acp`, not `claude-code-acp` (the package's former
        // name, before it moved under the `@agentclientprotocol` scope). A
        // stale `claude-code-acp` here silently fails every "not found" probe
        // and every spawn on a current install.
        command: "claude-agent-acp",
        args: &[],
        credential: Some(".claude/.credentials.json"),
    },
    Harness {
        id: "codex",
        label: "Codex",
        command: "codex-acp",
        args: &[],
        credential: Some(".codex/auth.json"),
    },
    Harness {
        id: "goose",
        label: "goose",
        command: "goose",
        args: &["acp"],
        credential: Some(".config/goose/config.yaml"),
    },
];

/// A harness and how ready it is.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub readiness: Readiness,
    /// Where the binary was found, when it was.
    pub path: Option<PathBuf>,
}

/// The environment a probe reads, so the rules below are testable without
/// installing anything.
pub trait Probe {
    /// The executable's location, or `None` when it is not on `PATH`.
    fn locate(&self, command: &str) -> Option<PathBuf>;
    /// The user's home directory, for credential lookups.
    fn home(&self) -> Option<PathBuf>;
    /// Whether a path exists.
    fn exists(&self, path: &Path) -> bool;
}

/// The real environment.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn locate(&self, command: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(command))
            .find(|candidate| is_executable(candidate))
    }

    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Reports every known harness and its readiness.
pub fn survey(probe: &dyn Probe) -> Vec<HarnessStatus> {
    HARNESSES.iter().map(|h| status_of(probe, h)).collect()
}

fn status_of(probe: &dyn Probe, harness: &Harness) -> HarnessStatus {
    let path = probe.locate(harness.command);
    let readiness = match &path {
        None => Readiness::NotInstalled,
        Some(_) => match harness.credential {
            // Nothing to sign in to: installed is ready.
            None => Readiness::Ready,
            Some(relative) => match probe.home() {
                // No home directory to look in. Reported as signed-out rather
                // than ready: claiming ready and failing at first use is the
                // worse of the two wrong answers, because it fails later and
                // further from the cause.
                None => Readiness::NotSignedIn,
                Some(home) if probe.exists(&home.join(relative)) => Readiness::Ready,
                Some(_) => Readiness::NotSignedIn,
            },
        },
    };
    HarnessStatus {
        id: harness.id,
        label: harness.label,
        readiness,
        path,
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashSet;

    struct Fake {
        installed: HashSet<String>,
        home: Option<PathBuf>,
        files: HashSet<PathBuf>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                installed: HashSet::new(),
                home: Some(PathBuf::from("/home/ada")),
                files: HashSet::new(),
            }
        }
        fn with_installed(mut self, command: &str) -> Self {
            self.installed.insert(command.to_string());
            self
        }
        fn with_file(mut self, path: &str) -> Self {
            self.files.insert(PathBuf::from(path));
            self
        }
        fn without_home(mut self) -> Self {
            self.home = None;
            self
        }
    }

    impl Probe for Fake {
        fn locate(&self, command: &str) -> Option<PathBuf> {
            self.installed
                .contains(command)
                .then(|| PathBuf::from(format!("/usr/local/bin/{command}")))
        }
        fn home(&self) -> Option<PathBuf> {
            self.home.clone()
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.contains(path)
        }
    }

    fn readiness_of(probe: &dyn Probe, id: &str) -> Readiness {
        survey(probe)
            .into_iter()
            .find(|s| s.id == id)
            .expect("a known harness")
            .readiness
    }

    #[test]
    fn a_missing_binary_reads_as_not_installed() {
        assert_eq!(
            readiness_of(&Fake::new(), "claude"),
            Readiness::NotInstalled
        );
    }

    #[test]
    fn an_installed_but_signed_out_harness_is_distinguished_from_a_missing_one() {
        // THE distinction this module exists for. Both are "unavailable", and
        // the fixes are completely different — install it, versus sign in — so
        // collapsing them tells the operator to do the wrong thing.
        let probe = Fake::new().with_installed("claude-agent-acp");
        assert_eq!(readiness_of(&probe, "claude"), Readiness::NotSignedIn);
        assert_ne!(readiness_of(&probe, "claude"), Readiness::NotInstalled);
    }

    #[test]
    fn a_signed_in_harness_is_ready() {
        let probe = Fake::new()
            .with_installed("claude-agent-acp")
            .with_file("/home/ada/.claude/.credentials.json");
        assert_eq!(readiness_of(&probe, "claude"), Readiness::Ready);
        assert!(readiness_of(&probe, "claude").is_ready());
    }

    #[test]
    fn each_harness_is_probed_at_its_own_paths() {
        // One harness being signed in must not make another look signed in.
        let probe = Fake::new()
            .with_installed("claude-agent-acp")
            .with_installed("codex-acp")
            .with_file("/home/ada/.claude/.credentials.json");
        assert_eq!(readiness_of(&probe, "claude"), Readiness::Ready);
        assert_eq!(readiness_of(&probe, "codex"), Readiness::NotSignedIn);
    }

    #[test]
    fn no_home_directory_reads_as_signed_out_rather_than_ready() {
        // The safe direction of the two wrong answers: claiming ready would
        // fail at first use, far from the cause.
        let probe = Fake::new()
            .with_installed("claude-agent-acp")
            .without_home();
        assert_eq!(readiness_of(&probe, "claude"), Readiness::NotSignedIn);
    }

    #[test]
    fn the_survey_reports_every_known_harness_even_when_none_are_installed() {
        // A settings pane has to be able to say "Codex: not installed". A list
        // that omitted missing harnesses would offer nothing to install.
        let statuses = survey(&Fake::new());
        assert_eq!(statuses.len(), HARNESSES.len());
        assert!(
            statuses
                .iter()
                .all(|s| s.readiness == Readiness::NotInstalled)
        );
        assert!(statuses.iter().all(|s| s.path.is_none()));
    }

    #[test]
    fn readiness_serialises_with_a_state_tag_the_console_can_switch_on() {
        let json = serde_json::to_value(Readiness::NotSignedIn).unwrap();
        assert_eq!(json["state"], "notSignedIn");
        let failed = serde_json::to_value(Readiness::SpawnFailed {
            reason: "exited immediately".into(),
        })
        .unwrap();
        assert_eq!(failed["state"], "spawnFailed");
        // The reason travels: "it didn't start" with no cause is not actionable.
        assert_eq!(failed["reason"], "exited immediately");
    }
}
