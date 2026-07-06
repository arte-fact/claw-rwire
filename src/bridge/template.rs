//! The "rwire app" workspace template (P3 — the roadmap's justified 5th module). A workspace
//! pre-wired to build rwire apps: the framework declared as a **GitHub git dep** (so an isola
//! lane fetches it anonymously over https — no SSH keys/creds in the sandbox), a previewable
//! hello-app skeleton, the rwire skill guide, review standards with the dogfood rule, and a
//! gaps doc. Mirrors `seed.rs`'s scaffold-then-overwrite pattern; git failures are non-fatal
//! (the files land regardless — the same best-effort stance as `write_brief`).

use std::path::Path;

use crate::gitops;
use crate::workspace;

/// Which starter a new workspace is scaffolded from. `Blank` is the historical behaviour
/// (a bare scaffold on first run); `RwireApp` lays down the paved road below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceTemplate {
    #[default]
    Blank,
    RwireApp,
}

impl WorkspaceTemplate {
    /// The stable slug used as the dashboard form value.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            WorkspaceTemplate::Blank => "blank",
            WorkspaceTemplate::RwireApp => "rwire-app",
        }
    }

    /// Parse a form value; anything unrecognised (including the blank default) is `Blank`.
    #[must_use]
    pub fn from_slug(slug: &str) -> Self {
        match slug {
            "rwire-app" => WorkspaceTemplate::RwireApp,
            _ => WorkspaceTemplate::Blank,
        }
    }
}

/// Lay the rwire-app paved road into a freshly created workspace: scaffold the standard dirs,
/// then overwrite the defaults with rwire-specific briefs and drop in a working, previewable
/// `projects/hello` starter committed on `develop` (so a request worktree cut from it sees the
/// app). Best-effort — a write or git failure is logged upstream, never fatal.
pub fn scaffold_rwire_app(data_dir: &Path, ws_id: &str, ws_name: &str) {
    let dir = workspace::dir_for(data_dir, ws_id);
    let _ = workspace::scaffold(&dir, ws_name);
    let _ = std::fs::write(dir.join("AGENT.md"), rwire_agent_md(ws_name));
    let _ = std::fs::write(dir.join("STANDARDS.md"), RWIRE_STANDARDS);
    let _ = std::fs::write(dir.join("skills").join("rwire.md"), RWIRE_SKILL);
    let _ = std::fs::write(dir.join("docs").join("rwire-gaps.md"), RWIRE_GAPS);

    let hello = dir.join("projects").join("hello");
    if std::fs::create_dir_all(hello.join("src")).is_ok() {
        let _ = std::fs::write(hello.join("Cargo.toml"), HELLO_CARGO);
        let _ = std::fs::write(hello.join("src").join("main.rs"), HELLO_MAIN);
        // Detection seeds the first pipeline.yaml draft (P4b): now that Cargo.toml is in place the
        // project detects as an rwire app, so the starter ships a readable checks/preview recipe.
        crate::pipeline::seed(&hello, crate::kinds::detect(&hello));
        // Birth the repo (init → .gitignore on `develop`), then commit the skeleton so it is
        // the branch base — otherwise a request worktree cut from `develop`'s HEAD wouldn't
        // carry the app files.
        let _ = gitops::ensure_project_repo(&hello);
        let _ = gitops::commit_all(&hello, None, "claw: scaffold rwire-app starter");
    }
}

/// The rwire framework guide, bundled from the sibling checkout at claw build time (claw
/// path-deps `../rwire`, so it is always present). Written to `skills/rwire.md` so agents read
/// the guide the operator maintains; refreshes whenever the framework guide is updated.
const RWIRE_SKILL: &str = include_str!("../../../rwire/.claude/skills/rwire/SKILL.md");

/// The workspace brief for an rwire-app workspace — replaces the generic scaffold `AGENT.md`.
fn rwire_agent_md(name: &str) -> String {
    format!(
        "# {name} — an rwire app workspace\n\n\
         This workspace builds **rwire** apps: self-contained Rust binaries that serve a \
         reactive web UI on one port, with no JS/CSS source and no build step beyond `cargo`.\n\n\
         `projects/hello` is a working, previewable starter — a live counter. Use it as the \
         reference for a new app, or evolve it into what's asked. Before writing or reviewing \
         any UI, **read `skills/rwire.md`** (the framework guide): components before markup, \
         `St`/`At` tokens before raw strings, the theme owns the aesthetic.\n\n\
         ## The preview contract\n\n\
         An rwire app serves from two environment variables claw injects for a preview — read \
         both in `main()` (the starter already does): `CLAW_BIND` (the bind address) → \
         `Server::bind`, and `CLAW_PREVIEW_BASE` (the mount prefix) → `.base_path`, set right \
         beside the bind. Without `.base_path`, the preview's live WebSocket connects to root \
         and 404s through the same-origin proxy.\n\n\
         ## Conventions\n\n\
         - `projects/` — one rwire app per directory (each its own `Cargo.toml`)\n\
         - `docs/` — write-ups, plus `rwire-gaps.md` (see the standards)\n\
         - `skills/` — `rwire.md`, the framework guide\n"
    )
}

/// The review standards for an rwire-app workspace: the base structure rules plus the rwire UI
/// conventions and the dogfood rule. Self-contained (does not depend on the generic default).
const RWIRE_STANDARDS: &str = r#"# Review standards

- Structure: split code into modules per subsystem; keep files under ~300 lines and functions
  under ~40 — reject monoliths even when they work.
- Tests live beside the code they cover and must pass (`cargo test`) before a pass verdict.
- Builds must be warning-clean (`cargo clippy --all-targets -- -D warnings`).
- Keep `pipeline.yaml` honest: when a change alters how the project builds, tests, or serves, the
  same pass must update the project's `pipeline.yaml` (its checks + preview recipe).
- Reject prose claims the diff does not back up.

## rwire UI

- **Components before markup.** Reach for `rwire_components` (Button, Card, Stack, Text, Input,
  Select, …) before hand-rolling elements. No hand-rolled chips, badges, dots, or composers.
- **Tokens before strings.** Attributes via `At`/`Av`, styles via `St` tokens — never a raw
  `.attr()`; `.style()` only for genuinely dynamic values. Colours via semantic `St` tokens
  (`St::BgApp`, `St::TextMuted`, …), never hardcoded hex.
- **The theme owns the aesthetic.** Set it once via `.theme(...)`; don't re-assert
  fonts/radius/borders per element.
- Wrap changing UI in `#[renderer]`; navigate with `Link::to(...)`, never a raw `<a href>`.

## The dogfood rule

rwire is ours. When the framework is missing a component or token, or forces an awkward
workaround, **record it in `docs/rwire-gaps.md`** — never paper over it by re-styling or
hand-rolling. The operator triages gaps upstream and re-pins the framework; a silent workaround
loses that signal.
"#;

/// The gaps-doc template — the dogfood loop's ledger.
const RWIRE_GAPS: &str = r#"# rwire gaps

Framework friction found while building here — one entry per gap, for upstream triage. Don't
work around a gap silently; log it so the operator can fix rwire and re-pin.

Format per entry:

## <short title>
- **Symptom:** what you were trying to do and what went wrong.
- **Missing/awkward API:** the rwire component, token, or method that should exist.
- **Workaround:** what you did instead, so the app still ships.

<!-- entries below -->
"#;

/// The starter app's manifest — rwire declared as a git dep from the public GitHub repo,
/// tracking `main` (the app's `Cargo.lock`, generated on first build, pins the resolved rev).
const HELLO_CARGO: &str = r#"[package]
name = "hello"
version = "0.1.0"
edition = "2021"

[dependencies]
rwire = { git = "https://github.com/arte-fact/rwire", branch = "main" }
rwire-components = { git = "https://github.com/arte-fact/rwire", branch = "main" }
rwire-themes = { git = "https://github.com/arte-fact/rwire", branch = "main" }
async-std = { version = "1.12", features = ["attributes"] }
"#;

/// The starter app: a live counter. Clicking +/- updates the count over the WebSocket, which
/// proves the preview's socket rides the same-origin proxy. Serves from `CLAW_BIND` (the port)
/// and `CLAW_PREVIEW_BASE` (the mount prefix), both read in `main()`; empty defaults make a
/// standalone `cargo run` a no-op.
const HELLO_MAIN: &str = r#"//! Hello — the rwire-app starter (a live counter).

use std::error::Error;

use async_std::main;
use rwire::capsule_gen::CapsuleConfig;
use rwire::theme::Theme;
use rwire::{handler, renderer, theme, ElementBuilder, Server, St, State};
use rwire_components::{
    Button, ButtonSize, Card, Container, ContainerSize, Gap, Stack, Text, TextColor,
};
use rwire_themes::palettes;

#[derive(State, Default)]
#[storage(memory)]
struct Counter {
    count: i32,
}

#[theme]
fn app_theme() -> Theme {
    Theme::dark().palette(palettes::nord())
}

#[main]
async fn main() -> Result<(), Box<dyn Error>> {
    // The port and the mount prefix both come from the environment — claw injects them for a
    // preview. `base_path` sits right beside `bind` so the served capsule's WebSocket connects
    // under the proxy mount, not root. Empty defaults make a plain `cargo run` a no-op.
    let bind = std::env::var("CLAW_BIND").unwrap_or_else(|_| "127.0.0.1:9000".to_string());
    let base = std::env::var("CLAW_PREVIEW_BASE").unwrap_or_default();
    Server::bind(&bind)?
        .base_path(&base)
        .root(app)
        .capsule_config(CapsuleConfig::new())
        .theme(app_theme())
        .run()
        .await
}

fn app() -> ElementBuilder {
    Container::new()
        .size(ContainerSize::Full)
        .padding(true)
        .child(
            Stack::centered()
                .child(
                    Card::new()
                        .child(
                            Stack::column()
                                .gap(Gap::Lg)
                                .align_center()
                                .children([
                                    Text::heading2("Hello from rwire").build(),
                                    render_count(),
                                    Stack::row()
                                        .gap(Gap::Md)
                                        .children([
                                            Button::secondary("-")
                                                .size(ButtonSize::Lg)
                                                .on_click(decrement()),
                                            Button::primary("+")
                                                .size(ButtonSize::Lg)
                                                .on_click(increment()),
                                        ])
                                        .build(),
                                ])
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
        .st([St::BgApp, St::MinHScreen])
}

#[renderer]
fn render_count(state: &Counter) -> ElementBuilder {
    Text::heading1(state.count.to_string())
        .color(TextColor::Accent)
        .build()
}

#[handler]
fn increment(state: &mut Counter) {
    state.count += 1;
}

#[handler]
fn decrement(state: &mut Counter) {
    state.count -= 1;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{self, ProjectKind};

    #[test]
    fn template_slugs_round_trip() {
        assert_eq!(
            WorkspaceTemplate::from_slug("rwire-app"),
            WorkspaceTemplate::RwireApp
        );
        assert_eq!(
            WorkspaceTemplate::from_slug("blank"),
            WorkspaceTemplate::Blank
        );
        // Anything unknown is the safe default.
        assert_eq!(
            WorkspaceTemplate::from_slug("nonsense"),
            WorkspaceTemplate::Blank
        );
        assert_eq!(WorkspaceTemplate::RwireApp.slug(), "rwire-app");
        assert_eq!(WorkspaceTemplate::default(), WorkspaceTemplate::Blank);
    }

    #[test]
    fn hello_manifest_detects_as_an_rwire_app() {
        // The scaffolded starter's git-dep manifest must classify as RwireApp so kind/CI/preview
        // are automatic.
        let dir = std::env::temp_dir().join(format!("claw-tmpl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("Cargo.toml"), HELLO_CARGO).expect("write");
        assert_eq!(kinds::detect(&dir), ProjectKind::RwireApp);
    }

    #[test]
    fn hello_main_wires_the_preview_contract() {
        // Both env vars and the base_path call must be present, or previews connect to root.
        assert!(HELLO_MAIN.contains("CLAW_BIND"));
        assert!(HELLO_MAIN.contains("CLAW_PREVIEW_BASE"));
        assert!(HELLO_MAIN.contains(".base_path(&base)"));
    }

    #[test]
    fn skill_guide_is_bundled() {
        // The include_str! must resolve to the real guide, not an empty file.
        assert!(RWIRE_SKILL.len() > 500);
        assert!(RWIRE_SKILL.contains("rwire"));
    }
}
