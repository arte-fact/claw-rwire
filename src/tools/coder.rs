//! The coder toolbox: `bash` / `read` / `write` / `edit`, scoped to an agent's
//! workspace. Added to a run only when its `tool_profile` is `Coder`. Unlike rust-claw,
//! the file tools resolve paths through [`workspace::jail`] — the workspace is an
//! *enforced* boundary, not a convention (`..` and absolute paths are rejected). `bash`
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

/// A toolbox bound to one workspace directory.
pub struct CoderToolbox {
    workspace: PathBuf,
}

impl CoderToolbox {
    #[must_use]
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
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
                Ok(command) => ToolOutcome::text(exec::bash(&self.workspace, command)),
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
        let path = self.resolve(str_arg(args, "path")?)?;
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

    /// Jail a model-supplied path into the workspace, turning an escape into a tool-error
    /// string (the loop continues; the model sees why).
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        workspace::jail(&self.workspace, path).map_err(|err| match err {
            WorkspaceError::Escape => "error: path escapes the workspace".to_owned(),
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
        (CoderToolbox::new(dir.clone()), dir)
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
            assert!(o.result.contains("escapes the workspace"), "{}", o.result);
        }
        let r = tb.call(READ, r#"{"path":"../../etc/passwd"}"#);
        assert!(!r.ok && r.result.contains("escapes the workspace"));
    }

    #[test]
    fn bad_json_is_a_tool_error_not_a_panic() {
        let (tb, _) = toolbox();
        let o = tb.call(WRITE, "not json");
        assert!(!o.ok);
        assert!(o.result.contains("invalid arguments"));
    }
}
