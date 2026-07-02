//! Per-agent workspaces: a directory under `<data>/workspaces/<folder>/` that the coder
//! tools (`bash`/`read`/`write`/`edit`) operate within, plus the `AGENT.md` that becomes
//! the agent's system prompt. Mirrors rust-claw's `groups/<folder>/` model, but scoped to
//! `workspaces/` and with the path boundary *enforced* (see [`path::jail`]).

pub mod ops;
mod path;

pub use ops::list_files;
pub use path::jail;

use std::path::{Path, PathBuf};

/// The largest text file the Files view will read into the editor (1 MiB). Larger files
/// are reported as `TooLarge` and shown read-only.
pub const FILE_VIEW_MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("path escapes the workspace")]
    Escape,
    /// The workspace root can't be located (cwd unreadable). Kept distinct from `Escape` so a
    /// setup problem isn't misreported to the model as "your path is outside the workspace" — the
    /// live symptom where an in-workspace absolute path was rejected as an escape.
    #[error("the workspace directory is unavailable")]
    RootUnavailable,
    #[error("no such file or directory")]
    NotFound,
    #[error("not a directory")]
    NotADirectory,
    #[error("not a file")]
    NotAFile,
    #[error("file is not valid UTF-8 text")]
    Binary,
    #[error("file is larger than the {FILE_VIEW_MAX_BYTES}-byte view limit")]
    TooLarge,
    #[error("the workspace root itself cannot be the target")]
    InvalidName,
    #[error("a file or directory already exists there")]
    Exists,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// The workspace directory for `folder` under `base` (the data dir): `base/workspaces/<folder>`.
#[must_use]
pub fn dir_for(base: &Path, folder: &str) -> PathBuf {
    base.join("workspaces").join(folder)
}

/// The standard top-level folders a workspace is organised into. Created on scaffold so the
/// layout exists up front; the `AGENT.md` brief documents what each is for, so executors put
/// new files where they belong (the "docs ended up in skills/" failure).
pub const STANDARD_DIRS: [&str; 3] = ["projects", "docs", "skills"];

/// Ensure the workspace exists, create the standard folders, and seed a starter `AGENT.md`
/// (the project brief) if one isn't there yet. Idempotent — an existing `AGENT.md` is never
/// overwritten and the directory creates are no-ops when they already exist.
pub fn scaffold(workspace: &Path, agent_name: &str) -> Result<(), WorkspaceError> {
    std::fs::create_dir_all(workspace)?;
    for dir in STANDARD_DIRS {
        std::fs::create_dir_all(workspace.join(dir))?;
    }
    let agent_md = workspace.join("AGENT.md");
    if !agent_md.exists() {
        std::fs::write(&agent_md, starter_agent_md(agent_name))?;
    }
    Ok(())
}

/// The default `AGENT.md` body for a freshly scaffolded workspace: a placeholder purpose line
/// plus the folder conventions, so a delegated task knows where its output should go.
#[must_use]
pub fn starter_agent_md(agent_name: &str) -> String {
    format!(
        "# {agent_name} workspace\n\n\
         Describe what this workspace is for — its goal, the project it owns, and any \
         standing instructions for the team.\n\n\
         ## Conventions\n\n\
         Keep the working tree tidy and put new files where they belong:\n\n\
         - `projects/` — cloned repositories and code projects\n\
         - `docs/` — documentation, reports, and write-ups\n\
         - `skills/` — reusable guides, snippets, and how-tos\n"
    )
}

/// A workspace-root markdown file, trimmed — `None` if missing, unreadable, or blank, so
/// callers fall back cleanly.
fn read_trimmed(workspace: &Path, name: &str) -> Option<String> {
    let content = std::fs::read_to_string(workspace.join(name)).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Read `AGENT.md` from the workspace — the shared project brief; absent, the run falls
/// back to the base prompt alone.
#[must_use]
pub fn read_agent_md(workspace: &Path) -> Option<String> {
    read_trimmed(workspace, "AGENT.md")
}

/// Read `STANDARDS.md` from the workspace — the validator standards memo (R5): curated
/// review rules, deliberately **not** a work log (validators stay memory-free).
#[must_use]
pub fn read_standards(workspace: &Path) -> Option<String> {
    read_trimmed(workspace, "STANDARDS.md")
}

/// The standards memo framed as a validator system-prompt layer, or `None` when unset.
#[must_use]
pub fn standards_layer(workspace: &Path) -> Option<String> {
    read_standards(workspace).map(|rules| {
        format!(
            "## Review standards\n\nCurated rules for this workspace — check the work \
             against them:\n\n{rules}"
        )
    })
}

/// Compose the run's layered system prompt, blank-line-separated, skipping empty layers:
/// the global `base`, then the agent's identity `agent_prompt` ("who am I", per-agent),
/// then the workspace `agent_md` project brief ("what we build", shared), then the agent's
/// rolling `memory` ("what's happened so far", Phase D). Identity precedes project context so
/// the persona frames the brief; memory comes last as accumulated background.
#[must_use]
pub fn compose_system_prompt(
    base: &str,
    agent_prompt: Option<String>,
    agent_md: Option<String>,
    memory: Option<String>,
) -> String {
    [Some(base.to_owned()), agent_prompt, agent_md, memory]
        .into_iter()
        .flatten()
        .map(|layer| layer.trim().to_owned())
        .filter(|layer| !layer.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A per-test scratch base — namespaced by test name so parallel tests can't wipe each
    /// other's trees (the shared-dir version did exactly that).
    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claw-ws-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn dir_for_nests_under_workspaces() {
        assert_eq!(
            dir_for(Path::new("/data"), "claw"),
            PathBuf::from("/data/workspaces/claw")
        );
    }

    #[test]
    fn scaffold_seeds_agent_md_and_standard_dirs_once() {
        let ws = dir_for(&tmp("scaffold"), "ag");
        scaffold(&ws, "Coder").expect("scaffold");
        let first = read_agent_md(&ws).expect("present");
        assert!(first.contains("Coder workspace"));
        assert!(first.contains("## Conventions"));
        // The standard folder layout exists up front.
        for dir in STANDARD_DIRS {
            assert!(ws.join(dir).is_dir(), "missing {dir}/");
        }

        // A second scaffold must not clobber an edited AGENT.md.
        std::fs::write(ws.join("AGENT.md"), "custom prompt").expect("write");
        scaffold(&ws, "Coder").expect("scaffold again");
        assert_eq!(read_agent_md(&ws).as_deref(), Some("custom prompt"));
    }

    #[test]
    fn empty_agent_md_reads_as_none() {
        let ws = dir_for(&tmp("blank"), "blank");
        std::fs::create_dir_all(&ws).expect("mkdir");
        std::fs::write(ws.join("AGENT.md"), "   \n\t").expect("write");
        assert_eq!(read_agent_md(&ws), None);
    }

    #[test]
    fn standards_memo_reads_and_wraps_as_a_review_layer() {
        let ws = dir_for(&tmp("standards"), "standards");
        std::fs::create_dir_all(&ws).expect("mkdir");
        // Missing / blank → no layer (the validator stays rules-free too).
        assert_eq!(standards_layer(&ws), None);
        std::fs::write(ws.join("STANDARDS.md"), "  \n").expect("write");
        assert_eq!(standards_layer(&ws), None);
        // Present → trimmed rules under the review-standards heading.
        std::fs::write(
            ws.join("STANDARDS.md"),
            "- tests must pass\n- no warnings\n",
        )
        .expect("write");
        let layer = standards_layer(&ws).expect("layer");
        assert!(layer.starts_with("## Review standards"));
        assert!(layer.contains("- tests must pass\n- no warnings"));
    }

    #[test]
    fn compose_joins_the_present_layers_in_identity_then_context_order() {
        assert_eq!(compose_system_prompt("base", None, None, None), "base");
        assert_eq!(
            compose_system_prompt("base", None, Some("brief".to_owned()), None),
            "base\n\nbrief"
        );
        assert_eq!(
            compose_system_prompt(
                "base",
                Some("persona".to_owned()),
                Some("brief".to_owned()),
                None
            ),
            "base\n\npersona\n\nbrief"
        );
        // Memory is appended last, after the brief.
        assert_eq!(
            compose_system_prompt("base", None, None, Some("memory".to_owned())),
            "base\n\nmemory"
        );
        // Empty layers (and an empty base) drop out rather than leaving blank gaps.
        assert_eq!(
            compose_system_prompt("", Some("   ".to_owned()), Some("brief".to_owned()), None),
            "brief"
        );
    }
}
