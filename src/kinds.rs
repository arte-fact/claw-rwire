//! Project kind: the paved-road bundle (P1). A project has no DB row — its identity is the
//! directory `gitops::repos` already lists, and its **kind** is a pure function over its
//! files. Kind binds the system-prompt layer (P1 — conventions + the machine contracts the
//! producer pre-empts) and, as later phases land, the CI recipe (P1b), preview command (P2),
//! and seeded skills. The only stored state is a sparse per-project override in `settings`,
//! written only when the operator overrides detection.

use std::path::Path;

use rusqlite::Connection;

use crate::db::settings;

/// One machine check in a kind's CI recipe (P1b): a stable `name` (the badge label + the
/// key results are stored under) and the shell `command` that judges the repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckSpec {
    pub name: &'static str,
    pub command: &'static str,
}

/// What a project *is*, and therefore how it's checked, previewed, and prompted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    /// A Rust app on the rwire framework — the flagship kind (P3). One binary, one port.
    RwireApp,
    /// A Rust binary/library that isn't rwire — cargo checks, no default preview.
    RustBin,
    /// A Node project (`package.json`) — npm checks + preview, base path via its own knob.
    NodeApp,
    /// Prose/reference — no build, no preview; checks are a link/format lint at most.
    Docs,
    /// Anything unrecognised — no recipe until the operator sets an override.
    Other,
}

impl ProjectKind {
    /// The stable slug used in settings keys, prompts, and the UI.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::RwireApp => "rwire-app",
            Self::RustBin => "rust-bin",
            Self::NodeApp => "node-app",
            Self::Docs => "docs",
            Self::Other => "other",
        }
    }

    /// Parse a slug back to a kind (operator override round-trip). Unknown → `None`.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug.trim() {
            "rwire-app" => Some(Self::RwireApp),
            "rust-bin" => Some(Self::RustBin),
            "node-app" => Some(Self::NodeApp),
            "docs" => Some(Self::Docs),
            "other" => Some(Self::Other),
            _ => None,
        }
    }

    /// The CI recipe a repo of this kind is judged by (P1b) — the machine checks run after a
    /// producer pass, before the model reviewer. Rust kinds get the cargo trio; non-cargo
    /// stacks return nothing here and rely on the per-repo `checks` override (the escape hatch).
    #[must_use]
    pub fn checks(self) -> &'static [CheckSpec] {
        match self {
            Self::RwireApp | Self::RustBin => &[
                CheckSpec {
                    name: "fmt",
                    command: "cargo fmt --check",
                },
                CheckSpec {
                    name: "clippy",
                    command: "cargo clippy --all-targets -- -D warnings",
                },
                CheckSpec {
                    name: "test",
                    command: "cargo test",
                },
            ],
            Self::NodeApp | Self::Docs | Self::Other => &[],
        }
    }

    /// The command that starts this kind's preview server (P2), run from the project dir with
    /// `CLAW_BIND=127.0.0.1:<port>` and `CLAW_PREVIEW_BASE` in the environment. `Some` only where
    /// the kind reliably implies a long-running server: an rwire app *is* one binary on one port,
    /// and a Node app typically serves. A plain `RustBin` may be a library or a one-shot CLI
    /// (`cargo run` errors on a lib), and Docs/Other aren't servers — those rely on the per-project
    /// `preview` override when they do serve, mirroring the `checks` escape hatch.
    #[must_use]
    pub fn preview_command(self) -> Option<&'static str> {
        match self {
            // Debug, not `--release`: the P1b checks build the debug profile, so `cargo run`
            // reuses that warm `target/` and starts in seconds — a `--release` preview would be a
            // separate from-scratch build. A preview is a dev artifact; fast startup wins.
            Self::RwireApp => Some("cargo run"),
            Self::NodeApp => Some("npm start"),
            Self::RustBin | Self::Docs | Self::Other => None,
        }
    }

    /// The system-prompt layer for a producer working in a project of this kind: its
    /// conventions plus the machine contracts the producer should pre-empt (running the
    /// checks itself is the cheapest review round). `None` for kinds with no guidance.
    #[must_use]
    pub fn prompt_layer(self) -> Option<String> {
        let body = match self {
            Self::RwireApp => {
                "This is an **rwire** project. Build the UI in Rust with the `el(...)` \
                 builder and `rwire_components`; never hand-roll HTML/CSS or reach for JS. \
                 Use semantic `St` style tokens, `#[renderer]`/`#[handler]` for reactive \
                 regions, and check `rwire_components` before hand-rolling a widget. It \
                 serves under `$CLAW_PREVIEW_BASE` when set."
            }
            Self::RustBin => {
                "This is a Rust project. Keep it warning-clean and tested: your pass will \
                 be judged by `cargo fmt --check`, `clippy -D warnings`, and `cargo test` \
                 — run them before finishing rather than leaving them for the reviewer."
            }
            Self::NodeApp => {
                "This is a Node project. It must build and pass `npm test`; when it serves, \
                 honour the base path in `$CLAW_PREVIEW_BASE` via the framework's own knob \
                 (Vite `base`, Next `basePath`, …)."
            }
            Self::Docs | Self::Other => return None,
        };
        Some(format!("## This project\n\n{body}"))
    }
}

/// Detect a project's kind from its files — pure, computed on read (no table, no
/// registration). rwire dependency in `Cargo.toml` ⇒ [`ProjectKind::RwireApp`]; any
/// `Cargo.toml` ⇒ [`ProjectKind::RustBin`]; `package.json` ⇒ [`ProjectKind::NodeApp`];
/// otherwise [`ProjectKind::Other`] (Docs is an operator override — prose isn't reliably
/// distinguishable from an empty scaffold).
#[must_use]
pub fn detect(project_dir: &Path) -> ProjectKind {
    let cargo = project_dir.join("Cargo.toml");
    if cargo.is_file() {
        let manifest = std::fs::read_to_string(&cargo).unwrap_or_default();
        return if manifest_uses_rwire(&manifest) {
            ProjectKind::RwireApp
        } else {
            ProjectKind::RustBin
        };
    }
    if project_dir.join("package.json").is_file() {
        return ProjectKind::NodeApp;
    }
    ProjectKind::Other
}

/// Whether a `Cargo.toml` declares an rwire dependency (a `rwire` or `rwire-components` key
/// anywhere in a `[dependencies]`-family table). Substring-free: matches the key at a line
/// start (optionally quoted) so a crate merely *named* in a comment doesn't count.
fn manifest_uses_rwire(manifest: &str) -> bool {
    manifest.lines().any(|line| {
        let key = line.trim_start().trim_start_matches('"');
        key.starts_with("rwire ")
            || key.starts_with("rwire=")
            || key.starts_with("rwire\"")
            || key.starts_with("rwire-components")
    })
}

/// The first `projects/<name>` path mentioned in a task spec — the repo the task works in,
/// used to resolve its kind for the prompt layer when the task isn't request-bound. Returns
/// the bare `<name>` (no `projects/` prefix). Pure; `None` when the spec names no project.
#[must_use]
pub fn project_in_spec(spec: &str) -> Option<String> {
    let idx = spec.find("projects/")?;
    let rest = &spec[idx + "projects/".len()..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The effective kind for a project: the operator's override if set, else detection.
#[must_use]
pub fn kind_for(conn: &Connection, workspace: &str, project: &str, dir: &Path) -> ProjectKind {
    if let Ok(Some(slug)) = settings::get(conn, &kind_key(workspace, project))
        && let Some(kind) = ProjectKind::from_slug(&slug)
    {
        return kind;
    }
    detect(dir)
}

/// `project.<ws>.<name>.kind` — the operator's kind override.
#[must_use]
pub fn kind_key(workspace: &str, project: &str) -> String {
    format!("project.{workspace}.{project}.kind")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("claw-kind-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn detects_rwire_over_plain_rust() {
        let dir = scratch("rwire");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"x\"\n\n[dependencies]\nrwire = { path = \"../rwire\" }\n",
        )
        .unwrap();
        assert_eq!(detect(&dir), ProjectKind::RwireApp);
    }

    #[test]
    fn detects_plain_rust_and_node_and_other() {
        let rust = scratch("rust");
        std::fs::write(rust.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_eq!(detect(&rust), ProjectKind::RustBin);

        let node = scratch("node");
        std::fs::write(node.join("package.json"), "{}").unwrap();
        assert_eq!(detect(&node), ProjectKind::NodeApp);

        assert_eq!(detect(&scratch("empty")), ProjectKind::Other);
    }

    #[test]
    fn rwire_key_not_fooled_by_a_comment() {
        let dir = scratch("comment");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"x\"\n# rwire is cool but unused\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        assert_eq!(detect(&dir), ProjectKind::RustBin);
    }

    #[test]
    fn slug_round_trips() {
        for kind in [
            ProjectKind::RwireApp,
            ProjectKind::RustBin,
            ProjectKind::NodeApp,
            ProjectKind::Docs,
            ProjectKind::Other,
        ] {
            assert_eq!(ProjectKind::from_slug(kind.slug()), Some(kind));
        }
        assert_eq!(ProjectKind::from_slug("nonsense"), None);
    }

    #[test]
    fn prompt_layer_matches_the_kind() {
        assert!(
            ProjectKind::RwireApp
                .prompt_layer()
                .unwrap()
                .contains("rwire")
        );
        assert!(
            ProjectKind::RustBin
                .prompt_layer()
                .unwrap()
                .contains("clippy")
        );
        assert!(ProjectKind::Docs.prompt_layer().is_none());
    }

    #[test]
    fn rust_kinds_carry_the_cargo_recipe_others_none() {
        let cargo: Vec<&str> = ProjectKind::RustBin
            .checks()
            .iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(cargo, ["fmt", "clippy", "test"]);
        assert_eq!(
            ProjectKind::RwireApp.checks(),
            ProjectKind::RustBin.checks()
        );
        assert!(ProjectKind::NodeApp.checks().is_empty());
        assert!(ProjectKind::Docs.checks().is_empty());
        assert!(ProjectKind::Other.checks().is_empty());
    }

    #[test]
    fn only_server_kinds_carry_a_default_preview() {
        assert_eq!(ProjectKind::RwireApp.preview_command(), Some("cargo run"));
        assert_eq!(ProjectKind::NodeApp.preview_command(), Some("npm start"));
        assert!(ProjectKind::RustBin.preview_command().is_none());
        assert!(ProjectKind::Docs.preview_command().is_none());
        assert!(ProjectKind::Other.preview_command().is_none());
    }

    #[test]
    fn overrides_win_over_detection() {
        let conn = crate::db::open_in_memory().expect("open");
        let dir = scratch("override");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_eq!(kind_for(&conn, "ws", "p", &dir), ProjectKind::RustBin);
        settings::set(&conn, &kind_key("ws", "p"), "docs").unwrap();
        assert_eq!(kind_for(&conn, "ws", "p", &dir), ProjectKind::Docs);
    }
}
