//! The coder toolbox: `bash` / `read` / `write` / `edit`, scoped to an agent's
//! workspace. Added to a run only when its `tool_profile` is `Coder`. Unlike rust-claw,
//! the file tools resolve paths through [`workspace::jail`] — the workspace is an
//! *enforced* boundary, not a convention (`..` and paths outside the workspace are rejected;
//! an absolute path is accepted only when it lands inside the workspace). `bash`
//! keeps `cwd = workspace` but is not jailed (it can reach anywhere — the sandbox seam is
//! left at the spawn point for a future `bwrap`/container).

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::workspace::{self, WorkspaceError};

use super::exec;
use super::{ToolDef, ToolOutcome, Toolbox};

pub const BASH: &str = "bash";
pub const READ: &str = "read";
pub const WRITE: &str = "write";
pub const EDIT: &str = "edit";

/// Default line cap for `read`, matching rust-claw.
const READ_LIMIT: usize = 2000;

/// A toolbox bound to one workspace directory. File tools always act on the host `workspace` dir;
/// `bash` runs through `exec` (host, or the workspace's isola sandbox — R6).
pub struct CoderToolbox {
    workspace: PathBuf,
    exec: exec::ExecStrategy,
}

impl CoderToolbox {
    /// Build with an explicit execution strategy — file tools always use the host `workspace`
    /// dir; `bash` runs through `exec` (host, or the workspace's isola sandbox).
    #[must_use]
    pub fn with_exec(workspace: PathBuf, exec: exec::ExecStrategy) -> Self {
        Self { workspace, exec }
    }
}

impl Toolbox for CoderToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        vec![
            def(
                BASH,
                "Run a bash command in the agent workspace. Returns exit code and output.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The shell command to run." }
                    },
                    "required": ["command"]
                }),
            ),
            def(
                READ,
                "Read a file with line numbers. Paths are relative to the workspace.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "offset": { "type": "integer", "description": "1-based first line to show." },
                        "limit": { "type": "integer", "description": "Max lines to show (default 2000)." }
                    },
                    "required": ["path"]
                }),
            ),
            def(
                WRITE,
                "Create or overwrite a file. Parent directories are created.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }),
            ),
            def(
                EDIT,
                "Replace an exact string in a file. old_string must match exactly once.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string" },
                        "new_string": { "type": "string" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            ),
        ]
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(format!("error: invalid arguments: {err}")),
        };
        match name {
            BASH => match str_arg(&args, "command") {
                Ok(command) => ToolOutcome::text(exec::bash_in(&self.exec, command)),
                Err(err) => ToolOutcome::error(err),
            },
            READ => outcome(self.read(&args)),
            WRITE => outcome(self.write(&args)),
            EDIT => outcome(self.edit(&args)),
            other => ToolOutcome::error(format!("error: no tool named {other}")),
        }
    }
}

impl CoderToolbox {
    fn read(&self, args: &Value) -> Result<String, String> {
        let path = self.locate(str_arg(args, "path")?)?;
        let content =
            std::fs::read_to_string(&path).map_err(|err| format!("error: cannot read: {err}"))?;
        let offset = usize_arg(args, "offset").unwrap_or(1).max(1);
        let limit = usize_arg(args, "limit").unwrap_or(READ_LIMIT);
        let mut out = String::new();
        for (idx, line) in content.lines().enumerate().skip(offset - 1).take(limit) {
            out.push_str(&format!("{:>6}\t{line}\n", idx + 1));
        }
        if out.is_empty() {
            return Ok("(no lines in range)".to_owned());
        }
        Ok(out)
    }

    fn write(&self, args: &Value) -> Result<String, String> {
        let path = self.resolve(str_arg(args, "path")?)?;
        let content = str_arg(args, "content")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("error: cannot create parent dirs: {err}"))?;
        }
        std::fs::write(&path, content).map_err(|err| format!("error: cannot write: {err}"))?;
        Ok(format!("wrote {} bytes", content.len()))
    }

    fn edit(&self, args: &Value) -> Result<String, String> {
        let path = self.resolve(str_arg(args, "path")?)?;
        let old = str_arg(args, "old_string")?;
        let new = str_arg(args, "new_string")?;
        if old.is_empty() {
            return Err("error: old_string must not be empty".to_owned());
        }
        let content =
            std::fs::read_to_string(&path).map_err(|err| format!("error: cannot read: {err}"))?;
        match content.matches(old).count() {
            0 => Err("error: old_string not found".to_owned()),
            1 => {
                let updated = content.replacen(old, new, 1);
                std::fs::write(&path, updated)
                    .map_err(|err| format!("error: cannot write: {err}"))?;
                Ok("edited 1 occurrence".to_owned())
            }
            n => Err(format!(
                "error: old_string matched {n} times, must be unique"
            )),
        }
    }

    /// Resolve a path to **read**: the jailed path if it exists, else a unique workspace file
    /// whose path ends with the request — so a reviewer's guessed `render.rs` finds the real
    /// `projects/…/render.rs` instead of failing. Ambiguous or no match → the jailed path (the
    /// read then errors clearly). Only `read` is forgiving; write/edit still demand an exact path.
    fn locate(&self, path: &str) -> Result<PathBuf, String> {
        let jailed = self.resolve(path)?;
        if jailed.exists() {
            return Ok(jailed);
        }
        Ok(self.find_by_suffix(path).unwrap_or(jailed))
    }

    /// A unique workspace file matching `raw` by path suffix (preferred) or, failing that, by
    /// basename. `None` if there's no match or it's ambiguous (so the caller doesn't guess wrong).
    fn find_by_suffix(&self, raw: &str) -> Option<PathBuf> {
        let needle = raw.trim().trim_start_matches("./").trim_matches('/');
        if needle.is_empty() {
            return None;
        }
        let files = workspace::list_files(&self.workspace, 5000);
        let suffix: Vec<&String> = files
            .iter()
            .filter(|rel| rel.as_str() == needle || rel.ends_with(&format!("/{needle}")))
            .collect();
        let rel = match suffix.as_slice() {
            [only] => Some((*only).clone()),
            [] => {
                let base = needle.rsplit('/').next().unwrap_or(needle);
                let by_base: Vec<&String> = files
                    .iter()
                    .filter(|rel| rel.rsplit('/').next() == Some(base))
                    .collect();
                match by_base.as_slice() {
                    [only] => Some((*only).clone()),
                    _ => None,
                }
            }
            _ => None,
        };
        rel.map(|rel| self.workspace.join(rel))
    }

    /// Jail a model-supplied path into the workspace, turning an escape into a tool-error
    /// string (the loop continues; the model sees why).
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        workspace::jail(&self.workspace, path).map_err(|err| match err {
            WorkspaceError::Escape => "error: path is outside the workspace — use a path inside \
                it, either relative to the workspace root (e.g. `projects/foo/src/lib.rs`) or an \
                absolute path that stays within the workspace"
                .to_owned(),
            other => format!("error: {other}"),
        })
    }
}

fn def(name: &str, description: &str, parameters: Value) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
    }
}

fn outcome(result: Result<String, String>) -> ToolOutcome {
    match result {
        Ok(text) => ToolOutcome::text(text),
        Err(err) => ToolOutcome::error(err),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("error: missing string argument {key:?}"))
}

fn usize_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolbox() -> (CoderToolbox, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("claw-coder-{}-{}", std::process::id(), fastid()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let exec = exec::ExecStrategy::Host { root: dir.clone() };
        (CoderToolbox::with_exec(dir.clone(), exec), dir)
    }

    fn fastid() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn advertises_the_four_coder_tools() {
        let (tb, _) = toolbox();
        let names: Vec<String> = tb.defs().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec![BASH, READ, WRITE, EDIT]);
    }

    #[test]
    fn write_then_read_round_trips() {
        let (tb, dir) = toolbox();
        let w = tb.call(WRITE, r#"{"path":"notes/hi.txt","content":"hello"}"#);
        assert!(w.ok, "{}", w.result);
        assert!(dir.join("notes/hi.txt").exists());
        let r = tb.call(READ, r#"{"path":"notes/hi.txt"}"#);
        assert!(r.ok);
        assert_eq!(r.result, "     1\thello\n");
    }

    #[test]
    fn edit_replaces_a_unique_string() {
        let (tb, _) = toolbox();
        tb.call(WRITE, r#"{"path":"a.txt","content":"foo bar foo"}"#);
        let dup = tb.call(
            EDIT,
            r#"{"path":"a.txt","old_string":"foo","new_string":"x"}"#,
        );
        assert!(!dup.ok, "ambiguous edit must fail");
        let ok = tb.call(
            EDIT,
            r#"{"path":"a.txt","old_string":"bar","new_string":"baz"}"#,
        );
        assert!(ok.ok, "{}", ok.result);
        let r = tb.call(READ, r#"{"path":"a.txt"}"#);
        assert_eq!(r.result, "     1\tfoo baz foo\n");
    }

    #[test]
    fn jail_rejects_escapes_for_every_file_tool() {
        let (tb, _) = toolbox();
        for args in [
            r#"{"path":"../escape.txt","content":"x"}"#,
            r#"{"path":"/etc/passwd","content":"x"}"#,
        ] {
            let o = tb.call(WRITE, args);
            assert!(!o.ok, "escape should be rejected: {args}");
            assert!(o.result.contains("outside the workspace"), "{}", o.result);
        }
        let r = tb.call(READ, r#"{"path":"../../etc/passwd"}"#);
        assert!(!r.ok && r.result.contains("outside the workspace"));
    }

    #[test]
    fn write_accepts_an_absolute_path_inside_the_workspace() {
        // Regression: a model that passes the workspace's own absolute path (as weaker models do,
        // echoing `pwd`/`ls`) must write to that file rather than getting "outside the workspace".
        let (tb, dir) = toolbox();
        let abs = dir.join("docs/report.md");
        let args = format!(
            "{{\"path\":{:?},\"content\":\"the report\"}}",
            abs.to_str().expect("utf-8")
        );
        let o = tb.call(WRITE, &args);
        assert!(o.ok, "{}", o.result);
        assert_eq!(
            std::fs::read_to_string(&abs).expect("written"),
            "the report"
        );
    }

    #[test]
    fn bad_json_is_a_tool_error_not_a_panic() {
        let (tb, _) = toolbox();
        let o = tb.call(WRITE, "not json");
        assert!(!o.ok);
        assert!(o.result.contains("invalid arguments"));
    }

    #[test]
    fn read_resolves_a_guessed_path_by_suffix() {
        let (tb, _) = toolbox();
        tb.call(
            WRITE,
            r#"{"path":"projects/rogue/src/render.rs","content":"fn render(){}"}"#,
        );
        // A reviewer's bare guess resolves to the unique real file (the #1 friction).
        let bare = tb.call(READ, r#"{"path":"render.rs"}"#);
        assert!(bare.ok, "{}", bare.result);
        assert!(bare.result.contains("fn render(){}"));
        // A partial suffix resolves too.
        let suffix = tb.call(READ, r#"{"path":"src/render.rs"}"#);
        assert!(suffix.ok, "{}", suffix.result);
    }

    #[test]
    fn read_does_not_guess_an_ambiguous_basename() {
        let (tb, _) = toolbox();
        tb.call(WRITE, r#"{"path":"a/mod.rs","content":"A"}"#);
        tb.call(WRITE, r#"{"path":"b/mod.rs","content":"B"}"#);
        // Two `mod.rs` — ambiguous, so no silent guess; the literal miss errors.
        let r = tb.call(READ, r#"{"path":"mod.rs"}"#);
        assert!(!r.ok, "ambiguous basename must not resolve: {}", r.result);
    }

    #[test]
    fn read_suffix_fallback_never_escapes_the_workspace() {
        let (tb, _) = toolbox();
        // A basename that only matches outside the jail must not resolve to anything.
        let r = tb.call(READ, r#"{"path":"passwd"}"#);
        assert!(!r.ok);
    }
}
