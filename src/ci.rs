//! CI (P1b): the machine judgement that runs before the model reviewer. Per-repo and
//! kind-driven — each repo a producer pass touched has its [`ProjectKind`](crate::kinds)
//! supply a check recipe (cargo for Rust kinds, nothing auto for other stacks), with a
//! per-repo `checks` settings override as the escape hatch for non-cargo builds. This module
//! is the runner + result type; the scheduler records the outcomes on the run and fails fast
//! on red before spending a reviewer round.
//!
//! Incremental builds come for free: Rust projects gitignore `/target`, and a request's lane
//! worktree persists across passes, so each repo's `target/` survives from one pass to the
//! next — no shared `CARGO_TARGET_DIR` (which the sandbox mount would fight anyway).

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::db::settings;
use crate::kinds;
use crate::tools::exec::{self, ExecStrategy};

/// CI wall-clock ceiling per check — a longer leash than interactive `bash` (a cold
/// `cargo test` is minutes, not the 120s bash cap). Incremental re-runs are far quicker.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(600);
/// Cap on a red check's stored failure tail — enough to see the error, not the whole log.
const TAIL_CAP_CHARS: usize = 2_000;

/// The outcome of one check on one repo — a row in `task_runs.checks` (JSON) and a badge on
/// the run entry. `repo` is lane-root-relative (`""` = the meta workspace repo).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckOutcome {
    pub repo: String,
    /// The check name — a [`CheckSpec`](crate::kinds::CheckSpec) name (`fmt`/`clippy`/`test`)
    /// or `custom` for an operator override.
    pub check: String,
    pub ok: bool,
    /// The failure tail when red; empty when green.
    pub tail: String,
}

/// Whether a set of outcomes is all-green (an empty set is vacuously green — a repo with no
/// recipe isn't "failing").
#[must_use]
pub fn all_green(outcomes: &[CheckOutcome]) -> bool {
    outcomes.iter().all(|o| o.ok)
}

/// Parse a run's stored `checks` JSON back to outcomes; `None`/malformed → empty (treated as
/// "no checks ran", which is green — the absence of judgement isn't a failure).
#[must_use]
pub fn outcomes_from_json(json: Option<&str>) -> Vec<CheckOutcome> {
    json.and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default()
}

/// Whether a run's stored checks are all-green (or absent). The macro gate and the
/// resume-to-review decision read this.
#[must_use]
pub fn json_all_green(json: Option<&str>) -> bool {
    all_green(&outcomes_from_json(json))
}

/// The producer feedback for a red pass (P1b fail-fast): the failed checks and their tails,
/// as a user turn the re-launched producer sees alongside its own replayed work. `None` when
/// the pass was green (or ran no checks) — nothing to fix.
#[must_use]
pub fn red_feedback(json: Option<&str>) -> Option<String> {
    let outcomes = outcomes_from_json(json);
    let red: Vec<&CheckOutcome> = outcomes.iter().filter(|o| !o.ok).collect();
    if red.is_empty() {
        return None;
    }
    let mut body = String::from(
        "Your pass failed its machine checks. Fix these before finishing — the reviewer \
         will see the same red result:\n",
    );
    for outcome in red {
        let where_ = if outcome.repo.is_empty() {
            outcome.check.clone()
        } else {
            format!("{} ({})", outcome.check, outcome.repo)
        };
        body.push_str(&format!("\n### {where_}\n{}\n", outcome.tail));
    }
    Some(body)
}

/// The red checks' names, deduped in order — the fail-fast feedback and the request badge's
/// `✗ fmt, test`.
#[must_use]
pub fn failed_names(outcomes: &[CheckOutcome]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for outcome in outcomes.iter().filter(|o| !o.ok) {
        let label = if outcome.repo.is_empty() {
            outcome.check.clone()
        } else {
            format!("{}:{}", outcome.repo, outcome.check)
        };
        if !names.contains(&label) {
            names.push(label);
        }
    }
    names
}

/// Run every touched repo's checks and collect the outcomes. `lane_root` is the task's lane
/// tree; `strategy` is the exec strategy built from that root (host or the lane's sandbox);
/// `repos` are the lane-root-relative repos the pass committed (from `parse_commits`). Checks
/// run in each repo's subdir (via `cd`, uniform across host + sandbox since both start at the
/// lane root). A repo with no recipe and no override contributes nothing.
#[must_use]
pub fn run_for_repos(
    conn: &Connection,
    workspace: &str,
    lane_root: &Path,
    strategy: &ExecStrategy,
    repos: &[String],
) -> Vec<CheckOutcome> {
    let mut outcomes = Vec::new();
    for repo in repos {
        let Ok(dir) = crate::gitops::repo_dir(lane_root, repo) else {
            continue;
        };
        for check in checks_for(conn, workspace, repo, &dir) {
            let command = if repo.is_empty() {
                check.command.clone()
            } else {
                format!("cd {} && {}", single_quote(repo), check.command)
            };
            let result = exec::run_in(strategy, &command, CHECK_TIMEOUT);
            let ok = passed(&result);
            outcomes.push(CheckOutcome {
                repo: repo.clone(),
                check: check.name,
                ok,
                tail: if ok { String::new() } else { tail(&result) },
            });
        }
    }
    outcomes
}

/// A resolved check for one repo — a name + a concrete command (the kind recipe's `&'static`
/// borrowed into `String`, or the operator's override string).
struct ResolvedCheck {
    name: String,
    command: String,
}

/// The checks for one repo: the base recipe — the project's `pipeline.yaml` (P4b), else the
/// operator's per-repo override (one `custom` check), else the repo's kind recipe — with the kind's
/// `fmt`·`test`·`build` **floor** merged in on top (P4b), so those always run and gate whatever the
/// file or override says. The meta repo (`""`) uses the workspace-level `checks` key (back-compat
/// with the pre-P1b global command); a project repo uses `…checks.<repo>`.
fn checks_for(conn: &Connection, workspace: &str, repo: &str, dir: &Path) -> Vec<ResolvedCheck> {
    let kind = if let Some(project) = repo.strip_prefix("projects/") {
        kinds::kind_for(conn, workspace, project, dir)
    } else {
        kinds::detect(dir)
    };
    let mut checks = resolved_checks(conn, workspace, repo, dir, kind);
    // The floor (P4b): the kind's fmt·test·build always run and gate, whatever the file or override
    // says — an agent can't weaken the gate by trimming a check. Appended when a floor command isn't
    // already resolved (matched by exact command).
    for spec in kind.floor() {
        if !checks.iter().any(|check| check.command == spec.command) {
            checks.push(ResolvedCheck {
                name: spec.name.to_owned(),
                command: spec.command.to_owned(),
            });
        }
    }
    checks
}

/// The base checks before the floor is merged: a project's `pipeline.yaml` (the source of truth,
/// P4b), else the per-repo override, else the kind recipe.
fn resolved_checks(
    conn: &Connection,
    workspace: &str,
    repo: &str,
    dir: &Path,
    kind: kinds::ProjectKind,
) -> Vec<ResolvedCheck> {
    if let Some(checks) = crate::pipeline::checks(dir) {
        return checks
            .into_iter()
            .map(|check| ResolvedCheck {
                name: check.name,
                command: check.run,
            })
            .collect();
    }
    let overridden = settings::get(conn, &override_key(workspace, repo))
        .ok()
        .flatten()
        .filter(|cmd| !cmd.trim().is_empty());
    if let Some(command) = overridden {
        return vec![ResolvedCheck {
            name: "custom".to_owned(),
            command,
        }];
    }
    kind.checks()
        .iter()
        .map(|spec| ResolvedCheck {
            name: spec.name.to_owned(),
            command: spec.command.to_owned(),
        })
        .collect()
}

/// The per-repo override key: `workspace.<ws>.checks` for the meta repo (the pre-P1b global
/// key), `workspace.<ws>.checks.<repo>` for a project.
#[must_use]
pub fn override_key(workspace: &str, repo: &str) -> String {
    if repo.is_empty() {
        format!("workspace.{workspace}.checks")
    } else {
        format!("workspace.{workspace}.checks.{repo}")
    }
}

/// Whether an [`exec::run_in`] result reports success — its first line is `exit 0`. A timeout
/// note or an `error:` line (never an `exit 0`) is a failure.
fn passed(result: &str) -> bool {
    result.lines().next() == Some("exit 0")
}

/// The last [`TAIL_CAP_CHARS`] of a red check's output — the failure signature, trimmed.
fn tail(result: &str) -> String {
    let trimmed = result.trim_end();
    let count = trimmed.chars().count();
    if count > TAIL_CAP_CHARS {
        let skip = count - TAIL_CAP_CHARS;
        let cut: String = trimmed.chars().skip(skip).collect();
        format!("…{cut}")
    } else {
        trimmed.to_owned()
    }
}

/// POSIX single-quote a path for a `cd` in a `bash -c` payload.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passed_reads_the_exit_line() {
        assert!(passed("exit 0"));
        assert!(passed("exit 0\nall tests passed"));
        assert!(!passed("exit 1\nerror[E0433]"));
        assert!(!passed("exit 101\ntest failed"));
        assert!(!passed("timed out after 600s — command killed"));
        assert!(!passed("error: sandbox unavailable"));
    }

    #[test]
    fn tail_keeps_the_end_not_the_start() {
        let long = format!("exit 1\n{}", "e".repeat(TAIL_CAP_CHARS + 500));
        let out = tail(&long);
        assert!(out.starts_with('…'), "elided prefix marker");
        assert!(out.chars().count() <= TAIL_CAP_CHARS + 1);
        assert_eq!(tail("exit 1\nboom"), "exit 1\nboom");
    }

    #[test]
    fn all_green_is_vacuously_true_when_empty() {
        assert!(all_green(&[]));
        assert!(all_green(&[outcome("", "fmt", true)]));
        assert!(!all_green(&[
            outcome("", "fmt", true),
            outcome("", "test", false)
        ]));
    }

    #[test]
    fn failed_names_dedupe_and_qualify_by_repo() {
        let outcomes = [
            outcome("", "fmt", true),
            outcome("projects/x", "clippy", false),
            outcome("projects/x", "test", false),
            outcome("", "test", false),
        ];
        assert_eq!(
            failed_names(&outcomes),
            ["projects/x:clippy", "projects/x:test", "test"]
        );
    }

    #[test]
    fn override_key_shape() {
        assert_eq!(override_key("studio", ""), "workspace.studio.checks");
        assert_eq!(
            override_key("studio", "projects/empire"),
            "workspace.studio.checks.projects/empire"
        );
    }

    #[test]
    fn checks_for_layers_override_and_kind_recipe_under_the_floor() {
        let conn = crate::db::open_in_memory().expect("open");
        let dir = std::env::temp_dir().join(format!("claw-ci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        // No override → the rust-bin kind recipe (fmt/clippy/test) with the build floor appended.
        let recipe: Vec<String> = checks_for(&conn, "ws", "projects/x", &dir)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(recipe, ["fmt", "clippy", "test", "build"]);

        // Override wins the base, but the floor (fmt·test·build) still runs — an override can't
        // drop the gate on a cargo project.
        settings::set(&conn, &override_key("ws", "projects/x"), "make ci").unwrap();
        let resolved: Vec<String> = checks_for(&conn, "ws", "projects/x", &dir)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(resolved, ["custom", "fmt", "test", "build"]);
    }

    #[test]
    fn the_floor_holds_even_when_pipeline_yaml_trims_it() {
        let conn = crate::db::open_in_memory().expect("open");
        let base = std::env::temp_dir().join(format!("claw-ci-floor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // A cargo project whose pipeline.yaml keeps only `test` still runs fmt + build (the floor);
        // the file's `test` is not duplicated.
        let cargo = base.join("cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        std::fs::write(cargo.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(cargo.join("pipeline.yaml"), "checks:\n  - cargo test\n").unwrap();
        let names: Vec<String> = checks_for(&conn, "ws", "projects/cargo", &cargo)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["test", "fmt", "build"]);

        // A non-cargo project has no floor: its pipeline.yaml checks stand alone (nothing forced).
        let docs = base.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("pipeline.yaml"),
            "checks:\n  - name: spell\n    run: aspell check\n",
        )
        .unwrap();
        let docs_names: Vec<String> = checks_for(&conn, "ws", "projects/docs", &docs)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(docs_names, ["spell"]);
    }

    fn outcome(repo: &str, check: &str, ok: bool) -> CheckOutcome {
        CheckOutcome {
            repo: repo.to_owned(),
            check: check.to_owned(),
            ok,
            tail: String::new(),
        }
    }

    // End-to-end runner check (runs real cargo — slow, so explicit). Proves the runner drives
    // the kind recipe, parses exit codes, and captures a red tail. Run with:
    //   cargo test -- --ignored runs_cargo_checks
    #[test]
    #[ignore = "runs cargo (fmt/clippy/test) — slow; run explicitly"]
    fn runs_cargo_checks_green_then_red() {
        let conn = crate::db::open_in_memory().expect("db");
        let dir = std::env::temp_dir().join(format!("claw-ci-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"ci_smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
        )
        .unwrap();
        // A clean, formatted, clippy-clean crate with a passing test.
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    \
             use super::*;\n\n    #[test]\n    fn adds() {\n        assert_eq!(add(2, 2), 4);\n    \
             }\n}\n",
        )
        .unwrap();
        let strategy = ExecStrategy::Host { root: dir.clone() };
        let green = run_for_repos(&conn, "ws", &dir, &strategy, &[String::new()]);
        assert_eq!(green.len(), 3, "fmt + clippy + test");
        assert!(all_green(&green), "a clean crate should pass: {green:?}");

        // A `return` where the tail expression suffices — clippy::needless_return, which
        // `-D warnings` turns into a hard error (red clippy).
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn add(a: i64, b: i64) -> i64 {\n    return a + b;\n}\n",
        )
        .unwrap();
        let red = run_for_repos(&conn, "ws", &dir, &strategy, &[String::new()]);
        assert!(
            !all_green(&red),
            "clippy -D warnings should catch it: {red:?}"
        );
        assert!(
            red.iter().any(|o| !o.ok && !o.tail.is_empty()),
            "a red check should capture its failure tail: {red:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
