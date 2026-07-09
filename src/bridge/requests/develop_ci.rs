//! Post-merge develop CI (the CD beat): after a merge that mutates `develop` — a delivery
//! (branch → develop) or a rollback (revert on develop) — re-run the touched repos' checks
//! against the canonical tree and record the outcome. The project page's develop pipeline then
//! reflects the branch's ACTUAL post-merge state instead of going stale, and a red develop is
//! surfaced immediately instead of discovered at the next release gate.
//!
//! Checks are minutes (600s/check cap), so the run happens on a worker thread with its own DB
//! connection; the worker posts [`Command::DevelopChecked`] back and the bridge thread persists
//! the outcomes as a synthetic **free-lane producer run** — an archived `done` task (invisible on
//! the board) + a `task_runs` row + `ci_logs` — which is exactly the shape the develop pipeline
//! panel (`free_lane_producer_runs`) and the logs drawer already read. No new read path.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use rwire::{ChangeSet, SharedServerState};

use crate::ci::{self, CheckOutcome};
use crate::db::{self, tasks};
use crate::state::App;

use super::super::{Command, data_dir, exec_strategy_for, log_line, send, surface_error};

/// Kick off the post-merge develop CI on a worker thread. `label` phrases the trigger for the
/// run title and notifications (`delivering "…"` / `rolling back "…"`); `repos` is the repos the
/// merge actually touched (lane-root-relative).
pub(crate) fn launch(workspace: String, request_id: String, label: String, repos: Vec<String>) {
    if repos.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(conn) = db::open(&data_dir().join("central.db")) else {
            return log_line(format!("develop-ci: could not open the DB after {label}"));
        };
        let root = crate::workspace::dir_for(&data_dir(), &workspace);
        let strategy = exec_strategy_for(&root);
        let outcomes = ci::run_for_repos(&conn, &workspace, &root, &strategy, &repos);
        send(Command::DevelopChecked {
            workspace,
            request_id,
            label,
            outcomes,
        });
    });
}

/// Persist a finished post-merge develop pass and surface it (bridge thread — owns the DB).
/// Green passes just appear in the develop pipeline (the delivery receipt already pinged);
/// a red develop pings the request's origin chat and raises the global error banner.
pub(crate) fn on_checked(
    conn: &Connection,
    shared: &SharedServerState,
    workspace: &str,
    request_id: &str,
    label: &str,
    outcomes: &[CheckOutcome],
) {
    if outcomes.is_empty() {
        // No repo resolved any checks — nothing ran, nothing to show.
        return log_line(format!("develop-ci: no checks configured after {label}"));
    }
    if let Err(err) = persist(conn, workspace, label, outcomes) {
        log_line(format!(
            "develop-ci: could not record the run after {label}: {err}"
        ));
    }
    if ci::all_green(outcomes) {
        log_line(format!("develop-ci: green after {label}"));
    } else {
        let failed = ci::failed_names(outcomes).join(", ");
        if let Ok(Some(request)) = db::requests::get(conn, request_id) {
            super::ping(
                conn,
                &request,
                &format!(
                    "⚠️ develop checks are red after {label}: {failed} — see the project \
                     page's pipeline."
                ),
            );
        }
        surface_error(
            shared,
            format!("develop checks red after {label}: {failed}"),
        );
    }
    // Refresh what the operator is looking at: the open project page (when it's one of the
    // touched projects) and the active workspace's board.
    if let Some((open_ws, open_project)) = open_project(shared)
        && open_ws == workspace
        && outcomes
            .iter()
            .any(|o| o.repo.strip_prefix("projects/") == Some(open_project.as_str()))
    {
        super::view::load_project_detail(conn, shared, workspace, &open_project, None);
    }
    super::super::tasks::project_if_active(conn, shared, workspace);
}

/// Record the pass as a synthetic free-lane producer run: an archived `done` task (never on the
/// board — the board filters `archived_at IS NULL`) whose sole run carries the checks JSON, plus
/// the full logs in `ci_logs` (FK onto the run) for the badge drawer.
fn persist(
    conn: &Connection,
    workspace: &str,
    label: &str,
    outcomes: &[CheckOutcome],
) -> Result<(), String> {
    let task_id = crate::id::new_id().map_err(|err| format!("mint task id: {err}"))?;
    let run_id = crate::id::new_id().map_err(|err| format!("mint run id: {err}"))?;
    let title = format!("CI · develop after {label}");
    let spec = format!("Post-merge develop checks, run automatically after {label}.");
    tasks::create(
        conn,
        &tasks::NewTask {
            id: &task_id,
            workspace_id: workspace,
            agent: None,
            title: &title,
            spec: &spec,
            schedule: None,
            origin_chat: None,
            max_retries: None,
        },
    )
    .map_err(|err| err.to_string())?;
    tasks::set_status(conn, &task_id, tasks::TaskStatus::Done).map_err(|err| err.to_string())?;
    tasks::archive(conn, &task_id).map_err(|err| err.to_string())?;
    let ok = ci::all_green(outcomes);
    let output = if ok {
        format!("develop is green after {label}.")
    } else {
        format!(
            "develop is RED after {label}: {}.",
            ci::failed_names(outcomes).join(", ")
        )
    };
    let checks_json = serde_json::to_string(outcomes).map_err(|err| err.to_string())?;
    db::task_runs::append(
        conn,
        &db::task_runs::NewRun {
            id: &run_id,
            task_id: &task_id,
            position: 0,
            role: "producer",
            agent: None,
            output: Some(&output),
            verdict: None,
            feedback: None,
            iteration: 0,
            ok,
            transcript: None,
            attempt: 1,
            memory: None,
            commits: None,
            checks: Some(&checks_json),
        },
    )
    .map_err(|err| err.to_string())?;
    for outcome in outcomes {
        if !outcome.log.is_empty() {
            let _ = db::ci_logs::insert(conn, &run_id, &outcome.repo, &outcome.check, &outcome.log);
        }
    }
    Ok(())
}

/// The project page currently open, read from the shared `App` — `(workspace_id, project)`, or
/// `None` when no project page is loaded. Read through the update hook with an empty `ChangeSet`
/// (nothing re-renders); there is no dedicated open-project slot to consult.
fn open_project(shared: &SharedServerState) -> Option<(String, String)> {
    let slot: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let out = Arc::clone(&slot);
    shared.update_shared_changed::<App>(ChangeSet::new(), move |app| {
        *out.lock().unwrap_or_else(|e| e.into_inner()) = app
            .project_detail
            .as_ref()
            .map(|detail| (detail.workspace_id.clone(), detail.name.clone()));
    });
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{open_in_memory, workspaces};

    fn outcome(repo: &str, check: &str, ok: bool) -> CheckOutcome {
        CheckOutcome {
            repo: repo.to_owned(),
            check: check.to_owned(),
            ok,
            tail: if ok { String::new() } else { "boom".to_owned() },
            log: format!("{check} full output"),
            duration_ms: 42,
        }
    }

    fn seeded() -> rusqlite::Connection {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "Studio").expect("ws");
        conn
    }

    #[test]
    fn a_persisted_pass_feeds_the_develop_pipeline_but_never_the_board() {
        let conn = seeded();
        let outcomes = [
            outcome("projects/hello", "fmt", true),
            outcome("projects/hello", "test", false),
        ];
        persist(&conn, "ws", "delivering \"Fix the parser\"", &outcomes).expect("persist");

        // The develop pipeline query (free-lane producer runs with checks) sees the run…
        let runs = db::task_runs::free_lane_producer_runs(&conn, "ws", 25).expect("runs");
        assert_eq!(runs.len(), 1);
        assert!(runs[0].task_title.contains("develop after"));
        let parsed = ci::outcomes_from_json(runs[0].checks.as_deref());
        assert_eq!(parsed.len(), 2);
        assert!(!ci::all_green(&parsed));

        // …the badge drawer can fetch each check's full log under the run's real id…
        let log = db::ci_logs::fetch(&conn, &runs[0].run_id, "projects/hello", "test")
            .expect("fetch")
            .expect("log row");
        assert_eq!(log, "test full output");

        // …and the synthetic task never appears on the board (archived).
        let board = tasks::list_for_workspace(&conn, "ws").expect("board");
        assert!(
            board.is_empty(),
            "synthetic CI task must stay off the board"
        );
    }

    #[test]
    fn a_green_pass_records_an_ok_run() {
        let conn = seeded();
        let outcomes = [outcome("projects/hello", "fmt", true)];
        persist(&conn, "ws", "rolling back \"Fix the parser\"", &outcomes).expect("persist");
        let runs = db::task_runs::free_lane_producer_runs(&conn, "ws", 25).expect("runs");
        assert_eq!(runs.len(), 1);
        assert!(ci::json_all_green(runs[0].checks.as_deref()));
    }
}
