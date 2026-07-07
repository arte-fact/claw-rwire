//! The agent-owned `pipeline.yaml` (P4b): a project's build · checks · preview recipe, kept as a
//! file the agents maintain. It is the **sole** source of truth for a project's CI checks
//! ([`crate::ci`]) and preview command ([`crate::bridge::preview`]): a parseable `pipeline.yaml` at
//! the project root defines them, and when it is absent the only fallback is the per-repo `checks`
//! override — there is no auto-detected recipe and no forced floor, so a project's pipeline is
//! exactly what its config says.
//!
//! Distinct from [`crate::db::pipeline`] — that is a workspace's workflow *stages* (the ordered
//! Build→Review steps a task flows through); this is a project's CI *recipe* (what commands run).

use std::path::Path;

use serde::Deserialize;

/// The recipe file claw reads at a project root.
pub const PIPELINE_FILE: &str = "pipeline.yaml";

/// A project's pipeline recipe, read from [`PIPELINE_FILE`]. Unknown keys are ignored, so a
/// project can carry extra fields without breaking resolution.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Pipeline {
    /// The ordered checks run after a producer pass, before the reviewer.
    #[serde(default)]
    pub checks: Vec<Check>,
    /// The preview server command; `None` when the project doesn't serve.
    #[serde(default)]
    pub preview: Option<String>,
}

/// One named check: a label (for the CI badge) plus the command to run. Parses from either a
/// mapping (`- name: fmt` / `run: cargo fmt --check`) or a bare command string (name derived).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub run: String,
}

impl<'de> Deserialize<'de> for Check {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bare(String),
            Named { name: String, run: String },
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Bare(run) => Check {
                name: derive_name(&run),
                run,
            },
            Raw::Named { name, run } => Check { name, run },
        })
    }
}

/// A short check name derived from a bare command: `cargo fmt --check` → `fmt`, `npm test` → `npm`.
fn derive_name(command: &str) -> String {
    let mut words = command.split_whitespace();
    match (words.next(), words.next()) {
        (Some("cargo"), Some(sub)) => sub.to_owned(),
        (Some(first), _) => first.to_owned(),
        (None, _) => "check".to_owned(),
    }
}

/// Parse the project's `pipeline.yaml`. `None` when the file is absent or unparseable — resolution
/// then falls back to the per-repo override, so a malformed edit degrades safely (the agent notices
/// when its checks don't change) rather than breaking the run.
#[must_use]
pub fn load(project_dir: &Path) -> Option<Pipeline> {
    let text = std::fs::read_to_string(project_dir.join(PIPELINE_FILE)).ok()?;
    serde_yml::from_str(&text).ok()
}

/// The checks a present, non-empty `pipeline.yaml` defines — the source of truth that wins over the
/// per-repo override. `None` falls back to the override.
#[must_use]
pub fn checks(project_dir: &Path) -> Option<Vec<Check>> {
    let pipeline = load(project_dir)?;
    (!pipeline.checks.is_empty()).then_some(pipeline.checks)
}

/// The preview command a present `pipeline.yaml` defines (blank/whitespace treated as unset).
/// `None` falls back to the per-project override.
#[must_use]
pub fn preview(project_dir: &Path) -> Option<String> {
    load(project_dir)?
        .preview
        .filter(|command| !command.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claw-pipeline-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write(dir: &Path, yaml: &str) {
        std::fs::write(dir.join(PIPELINE_FILE), yaml).expect("write pipeline");
    }

    #[test]
    fn named_and_bare_checks_parse_and_the_file_wins() {
        let dir = scratch("mixed");
        write(
            &dir,
            "checks:\n  - name: fmt\n    run: cargo fmt --check\n  - cargo test\npreview: cargo run\n",
        );
        let checks = checks(&dir).expect("checks present");
        assert_eq!(
            checks[0],
            Check {
                name: "fmt".to_owned(),
                run: "cargo fmt --check".to_owned(),
            }
        );
        // A bare string derives its name from the command.
        assert_eq!(
            checks[1],
            Check {
                name: "test".to_owned(),
                run: "cargo test".to_owned(),
            }
        );
        assert_eq!(preview(&dir).as_deref(), Some("cargo run"));
    }

    #[test]
    fn absent_or_malformed_yields_none_so_callers_fall_back() {
        let dir = scratch("absent");
        assert!(load(&dir).is_none(), "absent file");
        write(&dir, "checks: [oops\n");
        assert!(checks(&dir).is_none(), "malformed yaml falls back");
    }

    #[test]
    fn a_file_with_no_checks_does_not_win_the_checks_resolution() {
        let dir = scratch("previewonly");
        write(&dir, "preview: npm start\n");
        assert!(
            checks(&dir).is_none(),
            "no checks key → fall back for checks"
        );
        assert_eq!(preview(&dir).as_deref(), Some("npm start"));
    }
}
