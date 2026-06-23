//! Task board commands, run on the bridge thread (it owns the DB). Create / cancel a task
//! and project a workspace's lane into `App.tasks`. The scheduler (`super::scheduler`) drains
//! the queued tasks; this module is the manual create/cancel + the lane projection.

use rwire::{ChangeSet, SharedServerState};

use crate::db::tasks::{self, NewTask, TaskStatus};
use crate::db::{agents, workspaces};
use crate::state::{App, LaneSummary, TaskView};

/// A workspace's lane as view-models (assignee ids resolved to names), for projection.
pub fn lane_views(conn: &rusqlite::Connection, workspace: &str) -> Vec<TaskView> {
    let names = agent_names(conn);
    tasks::list_for_workspace(conn, workspace)
        .unwrap_or_default()
        .into_iter()
        .map(|task| TaskView {
            id: task.id.clone(),
            title: task.title,
            spec: task.spec,
            agent: task
                .agent
                .as_ref()
                .and_then(|id| names.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone())),
            agent_id: task.agent,
            status: task.status.as_str().to_owned(),
            recurrent: matches!(task.kind, tasks::TaskKind::Recurrent),
            schedule: task.schedule,
            next_run: task.next_run.as_deref().map(short_time),
            result: task.result.map(|text| snippet(&text)),
            blockers: tasks::blockers_of(conn, &task.id).unwrap_or_default(),
            step_position: task.step_position,
            iteration: task.iteration,
            auto_retries: task.auto_retries,
            max_retries: task.max_retries,
            attempt: task.attempt,
            // A pending task holding on a future `next_run` after a failure is awaiting a retry.
            next_retry: if task.status == TaskStatus::Pending && task.auto_retries > 0 {
                task.next_run.as_deref().map(short_time)
            } else {
                None
            },
        })
        .collect()
}

/// The workspace's pipeline step names — the kanban column headers (the default single Execute
/// stage when none configured).
fn columns_for(conn: &rusqlite::Connection, workspace: &str) -> Vec<String> {
    crate::db::pipeline::steps_for(conn, workspace)
        .unwrap_or_default()
        .into_iter()
        .map(|step| step.name)
        .collect()
}

/// Parse a run's persisted transcript JSON into tool cards for the detail view.
fn parse_transcript(json: Option<&str>) -> Vec<crate::state::ToolCardView> {
    let Some(json) = json else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            let string = |key: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            crate::state::ToolCardView {
                name: string("name"),
                arguments: string("arguments"),
                result: string("result"),
                ok: value
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                time: String::new(),
            }
        })
        .collect()
}

/// Map one persisted run to its view-model — agent id resolved to a name, transcript parsed into
/// tool cards. Shared by the detail run chain and the Review inbox.
fn run_view(
    names: &[(String, String)],
    run: crate::db::task_runs::TaskRun,
) -> crate::state::RunView {
    crate::state::RunView {
        agent: run.agent.as_ref().map_or_else(
            || "you".to_owned(),
            |id| {
                names
                    .iter()
                    .find(|(i, _)| i == id)
                    .map_or_else(|| id.clone(), |(_, n)| n.clone())
            },
        ),
        role: run.role,
        output: run.output.unwrap_or_default(),
        verdict: run.verdict,
        feedback: run.feedback,
        iteration: run.iteration,
        ok: run.ok,
        tools: parse_transcript(run.transcript.as_deref()),
        attempt: run.attempt,
    }
}

/// A plan's proposed (held) child tasks as view-models for the Review inbox (R3).
pub(super) fn proposed_tasks(
    conn: &rusqlite::Connection,
    plan_id: &str,
) -> Vec<crate::state::ProposedTask> {
    let names = agent_names(conn);
    tasks::children_of_plan(conn, plan_id)
        .unwrap_or_default()
        .into_iter()
        .map(|task| crate::state::ProposedTask {
            title: task.title,
            spec: task.spec,
            agent: task
                .agent
                .and_then(|id| names.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone())),
            schedule: task.schedule,
        })
        .collect()
}

/// The producer's latest successful run (current attempt) as a view-model — the work the Review
/// inbox shows, with its tool transcript. `None` if no such run was recorded.
pub(super) fn latest_producer_run_view(
    conn: &rusqlite::Connection,
    task_id: &str,
) -> Option<crate::state::RunView> {
    let task = tasks::get(conn, task_id).ok().flatten()?;
    let run = crate::db::task_runs::list_for_task_attempt(conn, task_id, task.attempt)
        .ok()?
        .into_iter()
        .rev()
        .find(|run| run.ok && (run.role == "stage" || run.role == "producer"))?;
    Some(run_view(&agent_names(conn), run))
}

/// Build one task's detail (spec + the full run chain). `None` if the task is gone.
fn build_detail(conn: &rusqlite::Connection, task_id: &str) -> Option<crate::state::TaskDetail> {
    let names = agent_names(conn);
    tasks::get(conn, task_id).ok().flatten().map(|task| {
        let runs = crate::db::task_runs::list_for_task(conn, task_id)
            .unwrap_or_default()
            .into_iter()
            .map(|run| run_view(&names, run))
            .collect();
        crate::state::TaskDetail {
            id: task.id,
            workspace_id: task.workspace_id,
            title: task.title,
            spec: task.spec,
            awaiting: task.status == tasks::TaskStatus::Awaiting,
            status: task.status.as_str().to_owned(),
            runs,
        }
    })
}

/// Project one task's detail into `App.task_detail` — the detail view opening (unconditional).
pub fn load_detail(conn: &rusqlite::Connection, shared: &SharedServerState, task_id: &str) {
    let detail = build_detail(conn, task_id);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_TASK_DETAIL]),
        move |app| app.task_detail = detail,
    );
}

/// Re-project the detail **only if it's the one currently open** — so a task finishing /
/// advancing updates the open detail live without hijacking a different task's view.
pub fn refresh_detail_if_open(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task_id: &str,
) {
    let detail = build_detail(conn, task_id);
    let task_id = task_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_TASK_DETAIL]),
        move |app| {
            if app.task_detail.as_ref().map(|d| d.id.as_str()) == Some(task_id.as_str()) {
                app.task_detail = detail;
            }
        },
    );
}

/// `2026-06-22T08:00:00.000Z` → `06-22 08:00` (drop seconds/millis for the card).
fn short_time(ts: &str) -> String {
    let date = ts.get(5..10).unwrap_or("");
    let time = ts.get(11..16).unwrap_or("");
    format!("{date} {time}").trim().to_owned()
}

/// First line of a result, capped, for the card's last-run snippet.
fn snippet(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    let capped: String = line.chars().take(120).collect();
    capped
}

/// Project a workspace's lane into `App.tasks` (unconditionally — the caller is acting on
/// the active workspace). Background updates use [`project_if_active`].
pub fn load(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let views = lane_views(conn, workspace);
    let columns = columns_for(conn, workspace);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_TASKS, App::FIELD_PIPELINE_COLUMNS]),
        move |app| {
            app.tasks = views;
            app.pipeline_columns = columns;
        },
    );
}

/// Re-project a lane into `App.tasks`, but only when the operator is actually viewing that
/// workspace — a background lane (a scheduler run, a planning turn) must not clobber the
/// lane on screen.
pub fn project_if_active(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let views = lane_views(conn, workspace);
    let columns = columns_for(conn, workspace);
    // Keep the Workflow editor's lock live: a task going running/awaiting (or finishing) must
    // flip the lock so the editor's controls reflect reality rather than going stale + dead.
    let locked = tasks::has_inflight(conn, workspace).unwrap_or(false);
    let workspace = workspace.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_TASKS,
            App::FIELD_PIPELINE_COLUMNS,
            App::FIELD_WORKFLOW_LOCKED,
        ]),
        move |app| {
            if app.active_workspace.as_deref() == Some(workspace.as_str()) {
                app.tasks = views;
                app.pipeline_columns = columns;
                app.workflow_locked = locked;
            }
        },
    );
}

/// Project every workspace's lane summary (running task + queued count) into
/// `App.dashboard_lanes` for the global dashboard's all-lanes view. Refreshed on each
/// scheduler tick and when the dashboard opens, so it stays live regardless of route.
pub fn project_dashboard(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let lanes: Vec<LaneSummary> = workspaces::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|ws| {
            let running = tasks::running_in(conn, &ws.id)
                .ok()
                .flatten()
                .map(|task| task.title);
            let lane = tasks::list_for_workspace(conn, &ws.id).unwrap_or_default();
            let queued = lane
                .iter()
                .filter(|task| task.status == TaskStatus::Pending)
                .count();
            let awaiting = lane
                .iter()
                .filter(|task| task.status == TaskStatus::Awaiting)
                .count();
            LaneSummary {
                workspace_id: ws.id,
                running,
                queued,
                awaiting,
            }
        })
        .collect();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_DASHBOARD_LANES]),
        move |app| app.dashboard_lanes = lanes,
    );
}

/// The create form's fields — the task the user is queuing from the New-task panel.
pub struct Create {
    pub workspace: String,
    pub title: String,
    pub spec: String,
    pub agent: Option<String>,
    pub schedule: Option<String>,
    pub max_retries: Option<i64>,
}

/// Create a task in its workspace, assigned to `agent` (a member; `None` = the owner), then
/// re-project the lane.
pub fn create(conn: &rusqlite::Connection, shared: &SharedServerState, new: Create) {
    let title = new.title.trim();
    let spec = new.spec.trim();
    if title.is_empty() || spec.is_empty() {
        return surface(shared, "a task needs a title and a spec".to_owned());
    }
    // A negative retry budget is meaningless — treat it as "use the global default".
    let max_retries = new.max_retries.filter(|n| *n >= 0);
    // A schedule, if given, must be a valid cron expression (else it would never fire).
    let schedule = new
        .schedule
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(cron) = &schedule
        && crate::schedule::Cron::parse(cron).is_none()
    {
        return surface(shared, format!("not a valid cron schedule: {cron:?}"));
    }
    let Ok(id) = crate::id::new_id() else {
        return surface(shared, "could not mint a task id".to_owned());
    };
    // A named assignee must be a member of the workspace; `None` stores NULL and defaults to
    // the owner at run time.
    let agent = new.agent.filter(|value| !value.trim().is_empty());
    if let Some(a) = &agent {
        let members = workspaces::members_of(conn, &new.workspace).unwrap_or_default();
        if !members.iter().any(|m| m == a) {
            return surface(
                shared,
                "the assignee must be a member of the workspace".into(),
            );
        }
    }
    if let Err(err) = tasks::create(
        conn,
        &NewTask {
            id: &id,
            workspace_id: &new.workspace,
            agent: agent.as_deref(),
            title,
            spec,
            schedule: schedule.as_deref(),
            origin_chat: None,
            max_retries,
        },
    ) {
        return surface(shared, format!("create task: {err}"));
    }
    load(conn, shared, &new.workspace);
}

/// An edit-drawer change to a task — the fields the user can rewrite from the board.
pub struct Edit {
    pub title: String,
    pub spec: String,
    pub agent: Option<String>,
    pub schedule: Option<String>,
    pub blockers: Vec<String>,
    /// Per-task auto-retry budget (`None` = use the global default).
    pub max_retries: Option<i64>,
}

/// Apply an [`Edit`] to a task, validating the assignee is a member, the schedule is valid
/// cron, and each blocker is a distinct task in the same workspace. Re-projects on success.
pub fn update(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str, edit: Edit) {
    let title = edit.title.trim();
    let spec = edit.spec.trim();
    if title.is_empty() || spec.is_empty() {
        return surface(shared, "a task needs a title and a spec".to_owned());
    }
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    // Editing a running task is a no-op on the live run (its messages were built at dispatch) —
    // refuse, so the user stops it first rather than silently editing nothing.
    if task.status == TaskStatus::Running {
        return surface(shared, "stop the task before editing it".to_owned());
    }
    let schedule = edit
        .schedule
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(cron) = &schedule
        && crate::schedule::Cron::parse(cron).is_none()
    {
        return surface(shared, format!("not a valid cron schedule: {cron:?}"));
    }
    let agent = edit.agent.filter(|value| !value.trim().is_empty());
    let members = workspaces::members_of(conn, &task.workspace_id).unwrap_or_default();
    if let Some(a) = &agent
        && !members.iter().any(|m| m == a)
    {
        return surface(
            shared,
            "the assignee must be a member of the workspace".into(),
        );
    }
    // Blockers: distinct from self, and tasks in the same workspace.
    let mut valid = Vec::new();
    for blocker in edit.blockers {
        if blocker == id {
            continue;
        }
        match tasks::get(conn, &blocker).ok().flatten() {
            Some(b) if b.workspace_id == task.workspace_id => valid.push(blocker),
            _ => {
                return surface(
                    shared,
                    format!("blocker {blocker:?} is not in this workspace"),
                );
            }
        }
    }
    let max_retries = edit.max_retries.filter(|n| *n >= 0);
    if let Err(err) = tasks::update(
        conn,
        id,
        title,
        spec,
        agent.as_deref(),
        schedule.as_deref(),
        max_retries,
    ) {
        return surface(shared, format!("update task: {err}"));
    }
    if let Err(err) = tasks::set_blockers(conn, id, &valid) {
        return surface(shared, format!("set blockers: {err}"));
    }
    load(conn, shared, &task.workspace_id);
}

/// Cancel a task and re-project its workspace's lane.
pub fn cancel(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::cancel(conn, id) {
        return surface(shared, format!("cancel task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Stop (pause) a task: park it `stopped`, abandon any in-flight run, and clear its live feed.
pub fn stop(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::stop(conn, id) {
        return surface(shared, format!("stop task: {err}"));
    }
    super::clear_live_text(shared, id);
    reproject(conn, shared, id, &task.workspace_id);
}

/// Resume a stopped task — re-queue it to continue from its current step.
pub fn resume(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::resume(conn, id) {
        return surface(shared, format!("start task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Restart a task from a clean slate (step 0, fresh attempt) — from a stopped or terminal task.
pub fn restart(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::restart(conn, id) {
        return surface(shared, format!("restart task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Re-project after a run-control change: the active lane, the open detail (if it's this task),
/// and the dashboard's all-lanes summary.
fn reproject(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str, workspace: &str) {
    load(conn, shared, workspace);
    refresh_detail_if_open(conn, shared, id);
    project_dashboard(conn, shared);
}

fn agent_names(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    agents::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect()
}

fn surface(shared: &SharedServerState, error: String) {
    super::log_line(format!("bridge tasks: {error}"));
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_ERROR]), move |app| {
        app.error = Some(error)
    });
}
