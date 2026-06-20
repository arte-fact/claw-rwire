//! Per-agent workspaces: a directory under `<data>/workspaces/<folder>/` that the coder
//! tools (`bash`/`read`/`write`/`edit`) operate within, plus the `AGENT.md` that becomes
//! the agent's system prompt. Mirrors rust-claw's `groups/<folder>/` model, but scoped to
//! `workspaces/` and with the path boundary *enforced* (see [`path::jail`]).

pub mod ops;
mod path;

pub use path::jail;

use std::path::{Path, PathBuf};

/// The largest text file the Files view will read into the editor (1 MiB). Larger files
/// are reported as `TooLarge` and shown read-only.
pub const FILE_VIEW_MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("path escapes the workspace")]
    Escape,
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

/// Ensure the workspace exists and seed a starter `AGENT.md` (the agent's persona) if one
/// isn't there yet. Idempotent — an existing `AGENT.md` is never overwritten.
pub fn scaffold(workspace: &Path, agent_name: &str) -> Result<(), WorkspaceError> {
    std::fs::create_dir_all(workspace)?;
    let agent_md = workspace.join("AGENT.md");
    if !agent_md.exists() {
        std::fs::write(&agent_md, starter_agent_md(agent_name))?;
    }
    Ok(())
}

/// The default `AGENT.md` body for a freshly scaffolded workspace.
#[must_use]
pub fn starter_agent_md(agent_name: &str) -> String {
    format!(
        "# {agent_name}\n\nYou are {agent_name}, a helpful assistant. Be concise and use \
         your tools when they help.\n"
    )
}

/// Read `AGENT.md` from the workspace, trimmed. `None` if it's missing, unreadable, or
/// only whitespace — the run then falls back to the base prompt alone.
#[must_use]
pub fn read_agent_md(workspace: &Path) -> Option<String> {
    let content = std::fs::read_to_string(workspace.join("AGENT.md")).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Write `AGENT.md` (the System-prompt editor in the drawer). Creates the workspace first.
pub fn write_agent_md(workspace: &Path, body: &str) -> Result<(), WorkspaceError> {
    std::fs::create_dir_all(workspace)?;
    std::fs::write(workspace.join("AGENT.md"), body)?;
    Ok(())
}

/// Compose the run's system prompt: the base prompt, then the agent's `AGENT.md` (if any),
/// separated by a blank line. Mirrors rust-claw's `compose_system_prompt`.
#[must_use]
pub fn compose_system_prompt(base: &str, agent_md: Option<String>) -> String {
    match (base.is_empty(), agent_md) {
        (true, Some(md)) => md,
        (false, Some(md)) => format!("{base}\n\n{md}"),
        (_, None) => base.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claw-ws-{}", std::process::id()));
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
    fn scaffold_seeds_agent_md_once_and_read_round_trips() {
        let ws = dir_for(&tmp(), "ag");
        scaffold(&ws, "Coder").expect("scaffold");
        let first = read_agent_md(&ws).expect("present");
        assert!(first.contains("You are Coder"));

        // A second scaffold must not clobber an edited AGENT.md.
        write_agent_md(&ws, "custom prompt").expect("write");
        scaffold(&ws, "Coder").expect("scaffold again");
        assert_eq!(read_agent_md(&ws).as_deref(), Some("custom prompt"));
    }

    #[test]
    fn empty_agent_md_reads_as_none() {
        let ws = dir_for(&tmp(), "blank");
        std::fs::create_dir_all(&ws).expect("mkdir");
        write_agent_md(&ws, "   \n\t").expect("write");
        assert_eq!(read_agent_md(&ws), None);
    }

    #[test]
    fn compose_joins_base_and_agent_md() {
        assert_eq!(compose_system_prompt("base", None), "base");
        assert_eq!(
            compose_system_prompt("base", Some("md".to_owned())),
            "base\n\nmd"
        );
        assert_eq!(compose_system_prompt("", Some("md".to_owned())), "md");
    }
}
