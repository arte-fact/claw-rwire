//! The coder toolbox: `bash` / `read` / `list` / `find` / `write` / `edit`, scoped to an agent's
//! workspace. Added to a run only when its `tool_profile` is `Coder`. Unlike rust-claw,
//! the file tools resolve paths through [`workspace::jail`] — the workspace is an
//! *enforced* boundary, not a convention (`..` and paths outside the workspace are rejected;
//! an absolute path is accepted only when it lands inside the workspace). `bash`
//! keeps `cwd = workspace` but is not jailed (it can reach anywhere — the sandbox seam is
//! left at the spawn point for a future `bwrap`/container).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::workspace::{self, WorkspaceError};

use super::exec;
use super::{ToolDef, ToolOutcome, Toolbox};

pub const BASH: &str = "bash";
pub const READ: &str = "read";
pub const LIST: &str = "list";
pub const FIND: &str = "find";
pub const WRITE: &str = "write";
pub const EDIT: &str = "edit";

/// Default line cap for `read`, matching rust-claw.
const READ_LIMIT: usize = 2000;

/// Cap on paths returned by `list`, so a huge tree can't blow up a prompt. Generous enough to
/// show a normal project in full; a truncated listing notes it.
const LIST_LIMIT: usize = 1000;

/// Cap on matches `find` returns, and on files it scans — bounds both prompt size and the work of
/// grepping every file, so a big tree can't stall a turn.
const FIND_LIMIT: usize = 200;
const FIND_SCAN_LIMIT: usize = 5000;
/// Longest `text`-match line `find` echoes back, so one giant minified line can't blow the budget.
const SNIPPET_MAX: usize = 200;

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
                LIST,
                "List files in the workspace (recursively), as workspace-relative paths you can \
                 pass to `read`. Give `path` to limit it to a subdirectory; omit it for the whole \
                 workspace. Use this to discover what's there instead of shelling out to `ls`.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Subdirectory to list (relative to the workspace). Omit for the whole workspace." }
                    }
                }),
            ),
            def(
                FIND,
                "Search the workspace for files by `name` (a substring of the path) and/or `text` \
                 (a substring inside files, like grep). Returns matching workspace-relative paths, \
                 with the first matching line for `text`. Give `path` to limit the search to a \
                 subdirectory. Use this instead of `bash` grep/find.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Case-insensitive substring to match against file paths." },
                        "text": { "type": "string", "description": "Substring to match inside file contents (grep)." },
                        "path": { "type": "string", "description": "Subdirectory to search within. Omit for the whole workspace." }
                    }
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
                // Git writes are fenced (the harness owns commits/branches/pushes); the
                // model sees why and which read-only paths remain open.
                Ok(command) => match super::git_screen::screen(command) {
                    Some(denied) => ToolOutcome::error(denied),
                    None => ToolOutcome::text(exec::bash_in(&self.exec, command)),
                },
                Err(err) => ToolOutcome::error(err),
            },
            READ => outcome(self.read(&args)),
            LIST => outcome(self.list(&args)),
            FIND => outcome(self.find(&args)),
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

    /// List workspace files (recursively), optionally under a subdirectory. Read-only: it never
    /// touches the filesystem outside the workspace and can't mutate anything — so an agent can be
    /// given `read` + `list` for full read access without `bash` (arbitrary command execution).
    fn list(&self, args: &Value) -> Result<String, String> {
        let prefix = self.scope_prefix(args)?;
        let all = workspace::list_files(&self.workspace, LIST_LIMIT);
        let rows: Vec<&String> = all.iter().filter(|path| under(&prefix, path)).collect();
        if rows.is_empty() {
            return Ok(if prefix.is_empty() {
                "(the workspace has no files yet)".to_owned()
            } else {
                format!("(no files under {prefix})")
            });
        }
        let mut out = rows
            .iter()
            .map(|path| path.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if all.len() == LIST_LIMIT {
            out.push_str(&format!("\n… (listing capped at {LIST_LIMIT} files)"));
        }
        Ok(out)
    }

    /// Search the workspace for files by `name` (case-insensitive substring of the path) and/or
    /// `text` (a substring inside the file — like `grep`), optionally scoped to a subdirectory with
    /// `path`. Returns matching workspace-relative paths, with the first matching `text` line. Like
    /// `list`, it's read-only, so `read` + `find` needs no `bash`.
    fn find(&self, args: &Value) -> Result<String, String> {
        let name = arg_str(args, "name").map(str::to_lowercase);
        let text = arg_str(args, "text");
        if name.is_none() && text.is_none() {
            return Err("error: give `name` (match file paths) and/or `text` \
                        (match file contents)"
                .to_owned());
        }
        let prefix = self.scope_prefix(args)?;
        let all = workspace::list_files(&self.workspace, FIND_SCAN_LIMIT);
        let mut out: Vec<String> = Vec::new();
        for rel in all.iter().filter(|path| under(&prefix, path)) {
            if let Some(needle) = name.as_deref()
                && !rel.to_lowercase().contains(needle)
            {
                continue;
            }
            match text {
                None => out.push(rel.clone()),
                // `text` given: read the file (skipping binary/unreadable) and report the first hit.
                Some(needle) => {
                    if let Ok(content) = std::fs::read_to_string(self.workspace.join(rel))
                        && let Some((line_no, line)) = content
                            .lines()
                            .enumerate()
                            .find(|(_, line)| line.contains(needle))
                    {
                        out.push(format!("{rel}:{}: {}", line_no + 1, snippet(line)));
                    }
                }
            }
            if out.len() >= FIND_LIMIT {
                break;
            }
        }
        if out.is_empty() {
            return Ok("(no matches)".to_owned());
        }
        let mut result = out.join("\n");
        if out.len() >= FIND_LIMIT {
            result.push_str(&format!("\n… (capped at {FIND_LIMIT} matches)"));
        }
        Ok(result)
    }

    /// The workspace-relative directory prefix a `list`/`find` targets (from the optional `path`
    /// arg), enforcing the jail — an escape or out-of-workspace absolute path is rejected. Empty
    /// string = the whole workspace.
    fn scope_prefix(&self, args: &Value) -> Result<String, String> {
        let requested = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if requested.is_empty() {
            return Ok(String::new());
        }
        let jailed = self.resolve(requested)?;
        Ok(relative_to_workspace(&self.workspace, &jailed)
            .unwrap_or_else(|| requested.trim_matches('/').to_owned()))
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

/// Whether a workspace-relative `path` sits under `prefix` (a directory prefix; empty = the whole
/// workspace). Shared by `list` and `find`.
fn under(prefix: &str, path: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// A trimmed, length-bounded view of a matched line for `find` output.
fn snippet(line: &str) -> String {
    let trimmed = line.trim();
    match trimmed.char_indices().nth(SNIPPET_MAX) {
        Some((end, _)) => format!("{}…", &trimmed[..end]),
        None => trimmed.to_owned(),
    }
}

/// A non-empty, trimmed string argument, or `None` when absent/blank.
fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// `target` (a jailed absolute path) expressed relative to the workspace `root`, forward-slashed —
/// matching the shape of [`workspace::list_files`] output for filtering. Tries the canonical root
/// first (an absolute request jails to a canonical path) then the root as given (a relative request
/// jails to `root.join(..)`); `None` if it isn't under either.
fn relative_to_workspace(root: &Path, target: &Path) -> Option<String> {
    let canon = root.canonicalize().ok();
    canon
        .as_deref()
        .and_then(|c| target.strip_prefix(c).ok())
        .or_else(|| target.strip_prefix(root).ok())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
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
    fn advertises_the_coder_tools() {
        let (tb, _) = toolbox();
        let names: Vec<String> = tb.defs().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec![BASH, READ, LIST, FIND, WRITE, EDIT]);
    }

    #[test]
    fn find_matches_by_name_and_by_content() {
        let (tb, _) = toolbox();
        tb.call(
            WRITE,
            r#"{"path":"projects/foo/src/lib.rs","content":"fn parse() { todo!() }\nlet x = 1;"}"#,
        );
        tb.call(
            WRITE,
            r#"{"path":"projects/foo/README.md","content":"docs for parse"}"#,
        );
        tb.call(
            WRITE,
            r#"{"path":"docs/notes.md","content":"nothing here"}"#,
        );

        // By name: substring of the path, case-insensitive.
        let by_name = tb.call(FIND, r#"{"name":"LIB.RS"}"#);
        assert!(by_name.ok, "{}", by_name.result);
        assert!(by_name.result.contains("projects/foo/src/lib.rs"));
        assert!(!by_name.result.contains("README"), "{}", by_name.result);

        // By content: grep, with the matching line and its number.
        let by_text = tb.call(FIND, r#"{"text":"todo!"}"#);
        assert!(
            by_text.result.contains("projects/foo/src/lib.rs:1:"),
            "{}",
            by_text.result
        );
        assert!(!by_text.result.contains("README"), "{}", by_text.result);

        // Scoped search: `path` limits the tree.
        let scoped = tb.call(FIND, r#"{"text":"parse","path":"projects"}"#);
        assert!(scoped.result.contains("projects/foo"));
        assert!(!scoped.result.contains("docs/"), "{}", scoped.result);

        // No matches / no criteria are handled cleanly.
        assert!(
            tb.call(FIND, r#"{"text":"zzznope"}"#)
                .result
                .contains("no matches")
        );
        let no_args = tb.call(FIND, "{}");
        assert!(!no_args.ok && no_args.result.contains("name"));
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
    fn list_enumerates_the_tree_and_filters_by_subdir() {
        let (tb, _) = toolbox();
        tb.call(WRITE, r#"{"path":"docs/report.md","content":"x"}"#);
        tb.call(WRITE, r#"{"path":"projects/foo/src/lib.rs","content":"y"}"#);
        tb.call(WRITE, r#"{"path":"projects/foo/Cargo.toml","content":"z"}"#);

        // No path → the whole workspace, workspace-relative, sorted.
        let all = tb.call(LIST, "{}");
        assert!(all.ok, "{}", all.result);
        for expected in [
            "docs/report.md",
            "projects/foo/src/lib.rs",
            "projects/foo/Cargo.toml",
        ] {
            assert!(
                all.result.contains(expected),
                "missing {expected} in {}",
                all.result
            );
        }

        // A subdir filters to just that subtree.
        let sub = tb.call(LIST, r#"{"path":"projects/foo"}"#);
        assert!(sub.result.contains("projects/foo/src/lib.rs"));
        assert!(
            !sub.result.contains("docs/report.md"),
            "should be filtered out: {}",
            sub.result
        );

        // An empty subtree says so rather than listing nothing silently.
        let empty = tb.call(LIST, r#"{"path":"skills"}"#);
        assert!(
            empty.ok && empty.result.contains("no files under skills"),
            "{}",
            empty.result
        );
    }

    #[test]
    fn list_accepts_an_absolute_in_workspace_path_and_rejects_escapes() {
        let (tb, dir) = toolbox();
        tb.call(WRITE, r#"{"path":"projects/foo/lib.rs","content":"y"}"#);
        // The workspace's own absolute path resolves to the same subtree.
        let abs = dir.join("projects/foo");
        let args = format!("{{\"path\":{:?}}}", abs.to_str().expect("utf-8"));
        let out = tb.call(LIST, &args);
        assert!(
            out.ok && out.result.contains("projects/foo/lib.rs"),
            "{}",
            out.result
        );
        // An escape is refused, like the other file tools.
        let esc = tb.call(LIST, r#"{"path":"../.."}"#);
        assert!(
            !esc.ok && esc.result.contains("outside the workspace"),
            "{}",
            esc.result
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
