//! The agent-owned `pipeline.yaml` (P4b): a project's build · checks · preview recipe, kept as a
//! file the agents maintain. When a parseable `pipeline.yaml` sits at the project root it is the
//! source of truth for the CI checks ([`crate::ci`]) and the preview command
//! ([`crate::bridge::preview`]); when absent, resolution falls back to the per-repo override and
//! then the kind recipe. claw still enforces a fmt·test·build floor regardless of the file (a
//! later P4b slice) so an agent can't weaken the gate by editing it away.
//!
//! Distinct from [`crate::db::pipeline`] — that is a workspace's workflow *stages* (the ordered
//! Build→Review steps a task flows through); this is a project's CI *recipe* (what commands run).

use std::path::Path;

use serde::Deserialize;

use crate::kinds::ProjectKind;

/// The recipe file claw reads at a project root.
pub const PIPELINE_FILE: &str = "pipeline.yaml";

/// A project's pipeline recipe, read from [`PIPELINE_FILE`]. Unknown keys are ignored, so a
/// forward `build:` key (consumed by the floor slice) parses harmlessly until then.
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
/// then falls back to the legacy override→kind path, so a malformed edit degrades safely (the
/// agent notices when its checks don't change) rather than breaking the run.
#[must_use]
pub fn load(project_dir: &Path) -> Option<Pipeline> {
    let text = std::fs::read_to_string(project_dir.join(PIPELINE_FILE)).ok()?;
    serde_yml::from_str(&text).ok()
}

/// The checks a present, non-empty `pipeline.yaml` defines — the source of truth that wins over the
/// per-repo override and the kind recipe. `None` falls back to legacy resolution.
#[must_use]
pub fn checks(project_dir: &Path) -> Option<Vec<Check>> {
    let pipeline = load(project_dir)?;
    (!pipeline.checks.is_empty()).then_some(pipeline.checks)
}

/// The preview command a present `pipeline.yaml` defines (blank/whitespace treated as unset).
/// `None` falls back to legacy resolution.
#[must_use]
pub fn preview(project_dir: &Path) -> Option<String> {
    load(project_dir)?
        .preview
        .filter(|command| !command.trim().is_empty())
}

/// A canonical `pipeline.yaml` draft for a freshly-detected `kind` — the "first draft" claw seeds
/// (P4b) so there's always a readable file to edit. `None` for kinds with nothing worth seeding
/// (no checks and no preview — the file would say nothing the fallback doesn't).
#[must_use]
pub fn seed_text(kind: ProjectKind) -> Option<String> {
    let checks = kind.checks();
    let preview = kind.preview_command();
    if checks.is_empty() && preview.is_none() {
        return None;
    }
    let mut out = String::from(
        "# claw pipeline — this project's checks + preview command.\n\
         # Edit freely; claw always runs a fmt·test·build floor regardless.\n",
    );
    if !checks.is_empty() {
        out.push_str("checks:\n");
        for spec in checks {
            out.push_str(&format!(
                "  - name: {}\n    run: {}\n",
                spec.name, spec.command
            ));
        }
    }
    if let Some(preview) = preview {
        out.push_str(&format!("preview: {preview}\n"));
    }
    Some(out)
}

/// Write [`seed_text`] to `project_dir/pipeline.yaml` when absent (best-effort) — never clobbers an
/// agent-owned file. Returns whether a file was written.
pub fn seed(project_dir: &Path, kind: ProjectKind) -> bool {
    let path = project_dir.join(PIPELINE_FILE);
    if path.exists() {
        return false;
    }
    match seed_text(kind) {
        Some(text) => std::fs::write(&path, text).is_ok(),
        None => false,
    }
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

    #[test]
    fn seeded_draft_round_trips_and_never_clobbers() {
        let dir = scratch("seed");
        assert!(seed(&dir, ProjectKind::RwireApp), "writes the first draft");
        assert!(
            !seed(&dir, ProjectKind::RwireApp),
            "never clobbers an existing (agent-owned) file"
        );
        let names: Vec<String> = checks(&dir)
            .expect("seeded checks parse back")
            .into_iter()
            .map(|check| check.name)
            .collect();
        assert_eq!(names, ["fmt", "clippy", "test"]);
        assert_eq!(preview(&dir).as_deref(), Some("cargo run"));

        // A kind with no recipe seeds nothing (the fallback already says it all).
        let empty = scratch("seed-empty");
        assert!(!seed(&empty, ProjectKind::Other));
        assert!(load(&empty).is_none());
    }
}
