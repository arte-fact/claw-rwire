//! Task board commands, run on the bridge thread (it owns the DB). Create / cancel a task
//! and project a workspace's lane into `App.tasks`. The scheduler (`super::scheduler`) drains
//! the queued tasks; this module is the manual create/cancel + the lane projection.

use rwire::{ChangeSet, SharedServerState};

use crate::db::tasks::{self, NewTask};
use crate::db::{agents, workspaces};
use crate::state::{App, TaskView};

/// A workspace's lane as view-models (assignee ids resolved to names), for projection.
pub fn lane_views(conn: &rusqlite::Connection, workspace: &str) -> Vec<TaskView> {
    let names = agent_names(conn);
    tasks::list_for_workspace(conn, workspace)
        .unwrap_or_default()
        .into_iter()
        .map(|task| TaskView {
            id: task.id,
            title: task.title,
            spec: task.spec,
            agent: task
                .agent
                .and_then(|id| names.iter().find(|(i, _)| i == &id).map(|(_, n)| n.clone())),
            status: task.status.as_str().to_owned(),
            recurrent: matches!(task.kind, tasks::TaskKind::Recurrent),
            schedule: task.schedule,
            next_run: task.next_run.map(|ts| short_time(&ts)),
            result: task.result.map(|text| snippet(&text)),
        })
        .collect()
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
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_TASKS]), move |app| {
        app.tasks = views
    });
}

/// Re-project a lane into `App.tasks`, but only when the operator is actually viewing that
/// workspace — a background lane (a scheduler run, a planning turn) must not clobber the
/// lane on screen.
pub fn project_if_active(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let views = lane_views(conn, workspace);
    let workspace = workspace.to_owned();
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_TASKS]), move |app| {
        if app.active_workspace.as_deref() == Some(workspace.as_str()) {
            app.tasks = views;
        }
    });
}

/// Create a task in `workspace`, assigned to `agent` (a member; `None` = the owner), then
/// re-project the lane.
pub fn create(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    title: &str,
    spec: &str,
    agent: Option<String>,
    schedule: Option<String>,
) {
    let title = title.trim();
    let spec = spec.trim();
    if title.is_empty() || spec.is_empty() {
        return surface(shared, "a task needs a title and a spec".to_owned());
    }
    // A schedule, if given, must be a valid cron expression (else it would never fire).
    let schedule = schedule
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
    let agent = agent.filter(|value| !value.trim().is_empty());
    if let Some(a) = &agent {
        let members = workspaces::members_of(conn, workspace).unwrap_or_default();
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
            workspace_id: workspace,
            agent: agent.as_deref(),
            title,
            spec,
            schedule: schedule.as_deref(),
            origin_chat: None,
        },
    ) {
        return surface(shared, format!("create task: {err}"));
    }
    load(conn, shared, workspace);
}

/// Cancel a task and re-project its workspace's lane.
pub fn cancel(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::cancel(conn, id) {
        return surface(shared, format!("cancel task: {err}"));
    }
    load(conn, shared, &task.workspace_id);
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
