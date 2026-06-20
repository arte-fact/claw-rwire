//! Blocking `bash` execution for the coder toolbox. Runs on the worker thread (no
//! tokio), with `cwd` pinned to the agent's workspace and a hard wall-clock timeout.
//!
//! Output is redirected to temp files rather than pipes: polling `try_wait` while a child
//! writes to a full pipe buffer would deadlock (the child blocks on write, never exits, so
//! the poll never sees it finish). Files have no such backpressure.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Per-command wall-clock cap (matches rust-claw's `BASH_TIMEOUT`).
pub const BASH_TIMEOUT: Duration = Duration::from_secs(120);
/// Cap on captured output fed back to the model, so a chatty command can't bloat the
/// transcript (and the DOM). Excess is truncated with a note.
const MAX_OUTPUT: usize = 32_000;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Run `command` via `bash -c` with `cwd = workspace`, returning `exit <code>\n<output>`
/// (stdout then stderr, combined). On timeout the child is killed and a note returned.
#[must_use]
pub fn bash(workspace: &Path, command: &str) -> String {
    run(workspace, command, BASH_TIMEOUT)
}

fn run(workspace: &Path, command: &str, timeout: Duration) -> String {
    let (out_path, err_path) = temp_paths();
    let (out_file, err_file) = match (
        std::fs::File::create(&out_path),
        std::fs::File::create(&err_path),
    ) {
        (Ok(o), Ok(e)) => (o, e),
        _ => return "error: bash could not create its output buffers".to_owned(),
    };

    let spawned = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(out_file)
        .stderr(err_file)
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            cleanup(&out_path, &err_path);
            return format!("error: failed to start bash: {err}");
        }
    };

    let start = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break format_result(status.code(), &out_path, &err_path),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let output = read_combined(&out_path, &err_path);
                    break format!(
                        "timed out after {}s — command killed\n{output}",
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                let _ = child.kill();
                break format!("error: waiting on bash failed: {err}");
            }
        }
    };

    cleanup(&out_path, &err_path);
    result
}

fn format_result(code: Option<i32>, out_path: &Path, err_path: &Path) -> String {
    let output = read_combined(out_path, err_path);
    let code = code.map_or_else(|| "killed".to_owned(), |c| c.to_string());
    if output.is_empty() {
        format!("exit {code}")
    } else {
        format!("exit {code}\n{output}")
    }
}

/// stdout then stderr, each lossy-decoded, joined, and truncated to [`MAX_OUTPUT`].
fn read_combined(out_path: &Path, err_path: &Path) -> String {
    let out = read_lossy(out_path);
    let err = read_lossy(err_path);
    let mut combined = out;
    if !err.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&err);
    }
    let trimmed = combined.trim_end();
    if trimmed.chars().count() > MAX_OUTPUT {
        let head: String = trimmed.chars().take(MAX_OUTPUT).collect();
        format!("{head}\n… (output truncated)")
    } else {
        trimmed.to_owned()
    }
}

fn read_lossy(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Unique temp paths for one command's stdout/stderr (pid + a process-wide counter, so
/// concurrent runs on different workers never collide).
fn temp_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    (
        dir.join(format!("claw-bash-{pid}-{n}.out")),
        dir.join(format!("claw-bash-{pid}-{n}.err")),
    )
}

fn cleanup(out_path: &Path, err_path: &Path) {
    let _ = std::fs::remove_file(out_path);
    let _ = std::fs::remove_file(err_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("claw-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn captures_stdout_and_exit_code() {
        let out = bash(&ws(), "echo hello");
        assert_eq!(out, "exit 0\nhello");
    }

    #[test]
    fn reports_nonzero_exit_and_stderr() {
        let out = bash(&ws(), "echo oops >&2; exit 3");
        assert_eq!(out, "exit 3\noops");
    }

    #[test]
    fn runs_in_the_workspace_cwd() {
        let dir = ws();
        let out = bash(&dir, "pwd");
        let real = dir.canonicalize().expect("canon");
        assert_eq!(out.trim(), format!("exit 0\n{}", real.display()));
    }

    #[test]
    fn kills_a_command_that_exceeds_the_timeout() {
        // A 10s sleep against a 1s cap must be killed promptly, not waited out.
        let start = Instant::now();
        let out = run(&ws(), "sleep 10", Duration::from_secs(1));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "should kill early"
        );
        assert!(out.starts_with("timed out after 1s"), "got: {out}");
    }
}
