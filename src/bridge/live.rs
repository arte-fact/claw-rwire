//! The live layer: run-lifecycle activity entries and the per-task streaming turn (text ↔
//! tool events in arrival order). Split from `bridge/mod.rs` — everything here writes the
//! `App` live fields under their own `ChangeSet`s; nothing touches the DB.

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::state::{ActivityEntry, ActivityKind, App};

use super::now_hm;

/// Newest-first, capped activity log.
const ACTIVITY_CAP: usize = 50;

pub(crate) fn push_activity(
    app: &mut App,
    title: String,
    kind: ActivityKind,
    detail: String,
    task_id: Option<String>,
    workspace_id: Option<String>,
) {
    app.activity.insert(
        0,
        ActivityEntry {
            time: now_hm(),
            title,
            kind,
            detail,
            task_id,
            workspace_id,
        },
    );
    app.activity.truncate(ACTIVITY_CAP);
}

/// Cap on the live agent-activity ring.
const LIVE_CAP: usize = 60;

/// Cap on concurrently-streaming tasks' live turns — a handful at most.
const LIVE_TURNS_CAP: usize = 8;

/// Fold a live agent event into both feeds: the global `App.live_activity` ring (the Activity
/// board) and the streaming task's ordered `App.live_turns` (the Thread's in-order live entry —
/// text segments and tool events interleaved as they arrive).
pub(crate) fn live_progress(
    shared: &SharedServerState,
    ctx: crate::state::ProgressCtx,
    event: agent::AgentEvent,
) {
    use crate::state::LiveEvent;
    use agent::AgentEvent;
    match event {
        AgentEvent::ToolStarted { name, arguments } => {
            let action = describe_tool(&name, &arguments);
            let time = now_hms();
            let entry = LiveEvent {
                time: time.clone(),
                agent: ctx.agent,
                workspace_id: ctx.workspace_id,
                workspace: ctx.workspace,
                task_id: ctx.task_id.clone(),
                task: ctx.task,
                action: action.clone(),
                in_progress: true,
                ok: false,
            };
            let task_id = ctx.task_id;
            shared.update_shared_changed::<App>(
                ChangeSet::from_fields(&[App::FIELD_LIVE_ACTIVITY, App::FIELD_LIVE_TURNS]),
                move |app| {
                    app.live_activity.insert(0, entry);
                    app.live_activity.truncate(LIVE_CAP);
                    turn_mut(app, &task_id).start_tool(time, action);
                },
            );
        }
        AgentEvent::ToolFinished { ok } => {
            let task_id = ctx.task_id;
            shared.update_shared_changed::<App>(
                ChangeSet::from_fields(&[App::FIELD_LIVE_ACTIVITY, App::FIELD_LIVE_TURNS]),
                move |app| {
                    if let Some(entry) = app
                        .live_activity
                        .iter_mut()
                        .find(|e| e.task_id == task_id && e.in_progress)
                    {
                        entry.in_progress = false;
                        entry.ok = ok;
                    }
                    if let Some(turn) = app.live_turns.iter_mut().find(|t| t.task_id == task_id) {
                        turn.finish_tool(ok);
                    }
                },
            );
        }
        AgentEvent::Text(chunk) => {
            let task_id = ctx.task_id;
            shared.update_shared_changed::<App>(
                ChangeSet::from_fields(&[App::FIELD_LIVE_TURNS]),
                move |app| turn_mut(app, &task_id).push_text(&chunk),
            );
        }
    }
}

/// The task's live turn, creating (and bounding) it on first use.
fn turn_mut<'a>(app: &'a mut App, task_id: &str) -> &'a mut crate::state::LiveTurn {
    if app.live_turns.iter().all(|turn| turn.task_id != task_id) {
        app.live_turns.push(crate::state::LiveTurn {
            task_id: task_id.to_owned(),
            items: Vec::new(),
        });
        if app.live_turns.len() > LIVE_TURNS_CAP {
            app.live_turns.remove(0);
        }
    }
    app.live_turns
        .iter_mut()
        .find(|turn| turn.task_id == task_id)
        .expect("turn ensured above")
}

/// Clear a task's live turn — its run is now recorded, so the persisted transcript (not the live
/// typewriter) is the source of truth.
pub(crate) fn clear_live_turn(shared: &SharedServerState, task_id: &str) {
    let task_id = task_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_LIVE_TURNS]),
        move |app| app.live_turns.retain(|turn| turn.task_id != task_id),
    );
}

/// A short neutral phrase for a tool call (the UI adds the ▷/✓/✗ prefix from the flags).
fn describe_tool(name: &str, arguments: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    let path = value.get("path").and_then(serde_json::Value::as_str);
    match name {
        "write" => format!("write {}", path.unwrap_or("a file")),
        "edit" => format!("edit {}", path.unwrap_or("a file")),
        "read" => format!("read {}", path.unwrap_or("a file")),
        "bash" => {
            let cmd = value
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let snippet: String = cmd.chars().take(48).collect();
            format!("bash: {snippet}")
        }
        "verdict" => "review".to_owned(),
        other => other.to_owned(),
    }
}

/// `HH:MM:SS` (UTC) — seconds matter for the live feed (events land seconds apart).
fn now_hms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}
