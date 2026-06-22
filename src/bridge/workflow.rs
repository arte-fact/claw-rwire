//! The pipeline editor (Phase F-5): add / update / remove / reorder a workspace's pipeline
//! steps. Edits are **locked while the workspace has in-flight tasks** (pending/running/
//! awaiting), so a running task never sees its pipeline shift under it. Projects
//! `App.workflow_steps` + `App.workflow_locked`; a pipeline change also re-projects the lane
//! (the kanban columns are the steps).

use rwire::{ChangeSet, SharedServerState};

use crate::db::pipeline::{self, StepFields};
use crate::db::tasks;
use crate::state::{App, StepForm, StepView};

/// Project the active workspace's configured steps + the edit lock.
pub fn load(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let steps: Vec<StepView> = pipeline::configured_steps(conn, workspace)
        .unwrap_or_default()
        .into_iter()
        .map(to_view)
        .collect();
    let locked = tasks::has_inflight(conn, workspace).unwrap_or(false);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_WORKFLOW_STEPS, App::FIELD_WORKFLOW_LOCKED]),
        move |app| {
            app.workflow_steps = steps;
            app.workflow_locked = locked;
        },
    );
}

/// Append a step of `kind` (`stage`/`loop`) with sensible defaults.
pub fn add(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str, kind: &str) {
    if locked(conn, workspace) {
        return load(conn, shared, workspace);
    }
    let configured = pipeline::configured_steps(conn, workspace).unwrap_or_default();
    let position = configured
        .iter()
        .map(|step| step.position)
        .max()
        .map_or(0, |max| max + 1);
    let id = crate::id::new_id().unwrap_or_default();
    let fields = if kind == "loop" {
        StepFields {
            kind: "loop",
            name: "Build & review",
            agent: None,
            prompt: None,
            validator_kind: Some("human"),
            validator_agent: None,
            validator_prompt: None,
            max_iterations: 2,
            on_exhaust: "human",
        }
    } else {
        StepFields {
            kind: "stage",
            name: "New stage",
            agent: None,
            prompt: None,
            validator_kind: None,
            validator_agent: None,
            validator_prompt: None,
            max_iterations: 1,
            on_exhaust: "human",
        }
    };
    let _ = pipeline::insert(conn, &id, workspace, position, &fields);
    finish(conn, shared, workspace);
}

/// Persist edited fields for a step (normalized by kind).
pub fn update(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    id: &str,
    form: &StepForm,
) {
    if locked(conn, workspace) {
        return load(conn, shared, workspace);
    }
    let is_loop = form.kind == "loop";
    let name = if form.name.trim().is_empty() {
        "Step"
    } else {
        form.name.trim()
    };
    let fields = StepFields {
        kind: if is_loop { "loop" } else { "stage" },
        name,
        agent: opt(&form.agent),
        prompt: opt(&form.prompt),
        validator_kind: if is_loop {
            Some(if form.validator_kind == "agent" {
                "agent"
            } else {
                "human"
            })
        } else {
            None
        },
        validator_agent: if is_loop {
            opt(&form.validator_agent)
        } else {
            None
        },
        validator_prompt: if is_loop {
            opt(&form.validator_prompt)
        } else {
            None
        },
        max_iterations: form.max_iterations.max(1),
        on_exhaust: if form.on_exhaust == "fail" {
            "fail"
        } else {
            "human"
        },
    };
    let _ = pipeline::update(conn, id, &fields);
    finish(conn, shared, workspace);
}

/// Delete a step.
pub fn remove(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str, id: &str) {
    if locked(conn, workspace) {
        return load(conn, shared, workspace);
    }
    let _ = pipeline::delete(conn, id);
    finish(conn, shared, workspace);
}

/// Move a step up or down by swapping positions with its neighbor.
pub fn move_step(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    id: &str,
    up: bool,
) {
    if locked(conn, workspace) {
        return load(conn, shared, workspace);
    }
    let steps = pipeline::configured_steps(conn, workspace).unwrap_or_default();
    if let Some(index) = steps.iter().position(|step| step.id == id) {
        let neighbor = if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1)
        };
        if let Some(other) = neighbor.filter(|&n| n < steps.len()) {
            let _ = pipeline::set_position(conn, &steps[index].id, steps[other].position);
            let _ = pipeline::set_position(conn, &steps[other].id, steps[index].position);
        }
    }
    finish(conn, shared, workspace);
}

fn locked(conn: &rusqlite::Connection, workspace: &str) -> bool {
    tasks::has_inflight(conn, workspace).unwrap_or(true)
}

/// Re-project the editor + the lane (the kanban columns are the steps) after a change.
fn finish(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    load(conn, shared, workspace);
    super::tasks::project_if_active(conn, shared, workspace);
}

fn opt(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn to_view(step: pipeline::Step) -> StepView {
    StepView {
        id: step.id,
        position: step.position,
        kind: match step.kind {
            pipeline::StepKind::Loop => "loop",
            pipeline::StepKind::Stage => "stage",
        }
        .to_owned(),
        name: step.name,
        agent: step.agent,
        prompt: step.prompt,
        validator_kind: step.validator_kind,
        validator_agent: step.validator_agent,
        validator_prompt: step.validator_prompt,
        max_iterations: step.max_iterations,
        on_exhaust: step.on_exhaust,
    }
}
