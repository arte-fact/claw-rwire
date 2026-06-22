//! The workspace owner's **planning** toolbox, used during the planning turn a `Command::Ask`
//! spins up: it turns a delegated request into task cards on the owner's lane. Scoped to one
//! workspace — `create_task` / `list_tasks` / `cancel_task` all act within it, and the
//! assignee enum is exactly that workspace's members (membership is the file-access boundary).
//!
//! Unlike the coder/MCP toolboxes this one owns a `rusqlite` connection (behind a `Mutex`, so
//! the box stays `Sync`): the planning turn runs on a worker thread without the bridge's DB
//! handle, so it reads/writes the board through its own connection (WAL → concurrent with the
//! bridge).

use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::tasks::{self, NewTask};
use crate::db::{agent_prompts, agents, workspaces};
use crate::id;

use super::{ToolDef, ToolOutcome, Toolbox};

pub const CREATE_TASK: &str = "create_task";
pub const LIST_TASKS: &str = "list_tasks";
pub const CANCEL_TASK: &str = "cancel_task";

/// One assignable member of the workspace, with the persona (identity prompt) the model
/// routes by.
struct Member {
    id: String,
    name: String,
    persona: String,
}

/// A planning toolbox bound to one workspace and its own DB connection.
pub struct PlanningToolbox {
    conn: Mutex<Connection>,
    workspace: String,
}

impl PlanningToolbox {
    #[must_use]
    pub fn new(conn: Connection, workspace: impl Into<String>) -> Self {
        Self {
            conn: Mutex::new(conn),
            workspace: workspace.into(),
        }
    }

    /// The workspace's members as routing candidates (id + name + persona snippet).
    fn members(&self, conn: &Connection) -> Vec<Member> {
        let ids = workspaces::members_of(conn, &self.workspace).unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| {
                let agent = agents::get(conn, &id).ok().flatten()?;
                let persona = agent_prompts::get(conn, &id)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Some(Member {
                    id: agent.id,
                    name: agent.name,
                    persona,
                })
            })
            .collect()
    }

    fn create(&self, conn: &Connection, args: &Value) -> ToolOutcome {
        let title = field(args, "title");
        let spec = field(args, "spec");
        if title.is_empty() || spec.is_empty() {
            return ToolOutcome::error("error: create_task needs a non-empty title and spec");
        }
        let members = self.members(conn);
        // An assignee, if named, must be a member; absent → the owner (NULL).
        let agent_id = match args.get("agent").and_then(Value::as_str) {
            Some(name) if !name.trim().is_empty() => {
                match members.iter().find(|m| m.name == name.trim()) {
                    Some(member) => Some(member.id.clone()),
                    None => {
                        return ToolOutcome::error(format!(
                            "error: {name:?} is not a member of this workspace"
                        ));
                    }
                }
            }
            _ => None,
        };
        let schedule = args
            .get("schedule")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let blockers = blocker_ids(args);
        // Blockers must be tasks in this same workspace (cross-workspace would couple lanes).
        for blocker in &blockers {
            match tasks::get(conn, blocker).ok().flatten() {
                Some(task) if task.workspace_id == self.workspace => {}
                _ => {
                    return ToolOutcome::error(format!(
                        "error: blocker {blocker:?} is not a task in this workspace"
                    ));
                }
            }
        }
        let Ok(new_id) = id::new_id() else {
            return ToolOutcome::error("error: could not mint a task id");
        };
        if let Err(err) = tasks::create(
            conn,
            &NewTask {
                id: &new_id,
                workspace_id: &self.workspace,
                agent: agent_id.as_deref(),
                title: title.trim(),
                spec: spec.trim(),
                schedule,
                origin_chat: None,
            },
        ) {
            return ToolOutcome::error(format!("error: create_task failed: {err}"));
        }
        for blocker in &blockers {
            let _ = tasks::add_blocker(conn, &new_id, blocker);
        }
        ToolOutcome::text(format!("queued #{new_id}: {}", title.trim()))
    }

    fn list(&self, conn: &Connection) -> ToolOutcome {
        let names: Vec<(String, String)> = agents::list(conn)
            .unwrap_or_default()
            .into_iter()
            .map(|agent| (agent.id, agent.name))
            .collect();
        let lines: Vec<String> = tasks::list_for_workspace(conn, &self.workspace)
            .unwrap_or_default()
            .into_iter()
            .map(|task| {
                let who = task
                    .agent
                    .and_then(|id| names.iter().find(|(i, _)| i == &id).map(|(_, n)| n.clone()))
                    .unwrap_or_else(|| "owner".to_owned());
                format!(
                    "#{} [{}] {} → {who}",
                    task.id,
                    task.status.as_str(),
                    task.title
                )
            })
            .collect();
        if lines.is_empty() {
            ToolOutcome::text("(no tasks in this workspace yet)")
        } else {
            ToolOutcome::text(lines.join("\n"))
        }
    }

    fn cancel(&self, conn: &Connection, args: &Value) -> ToolOutcome {
        let id = field(args, "id");
        if id.is_empty() {
            return ToolOutcome::error("error: cancel_task needs a task id");
        }
        match tasks::get(conn, id).ok().flatten() {
            Some(task) if task.workspace_id == self.workspace => {
                if let Err(err) = tasks::cancel(conn, id) {
                    return ToolOutcome::error(format!("error: cancel_task failed: {err}"));
                }
                ToolOutcome::text(format!("cancelled #{id}"))
            }
            _ => ToolOutcome::error(format!("error: no task {id:?} in this workspace")),
        }
    }
}

impl Toolbox for PlanningToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        let roster = {
            let conn = self.conn.lock().expect("planning conn");
            self.members(&conn)
        };
        let member_names: Vec<String> = roster.iter().map(|m| m.name.clone()).collect();
        let roster_desc = if roster.is_empty() {
            "This workspace has no members yet; tasks go to the owner.".to_owned()
        } else {
            let lines: Vec<String> = roster
                .iter()
                .map(|m| {
                    let persona = m.persona.lines().next().unwrap_or("").trim();
                    if persona.is_empty() {
                        format!("- {}", m.name)
                    } else {
                        format!("- {} — {persona}", m.name)
                    }
                })
                .collect();
            format!("Members you can assign:\n{}", lines.join("\n"))
        };
        let mut agent_schema = json!({
            "type": "string",
            "description": "Assignee: a workspace member by name; omit to assign the owner.",
        });
        if !member_names.is_empty() {
            agent_schema["enum"] = json!(member_names);
        }
        vec![
            ToolDef {
                name: CREATE_TASK.to_owned(),
                description: format!(
                    "Queue a task on this workspace's lane. {roster_desc}\nUse `schedule` (a \
                     cron expression) for a recurring task, and `blockers` (task ids in this \
                     workspace) for one that must wait."
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Short card label." },
                        "spec": { "type": "string", "description": "The instruction the assignee runs." },
                        "agent": agent_schema,
                        "schedule": { "type": "string", "description": "Cron expression for a recurring task; omit for one-shot." },
                        "blockers": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Ids of tasks in this workspace that must finish first."
                        }
                    },
                    "required": ["title", "spec"]
                }),
            },
            ToolDef {
                name: LIST_TASKS.to_owned(),
                description: "List this workspace's tasks (id, status, title, assignee)."
                    .to_owned(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            ToolDef {
                name: CANCEL_TASK.to_owned(),
                description: "Cancel a task in this workspace by id.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
        ]
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let conn = self.conn.lock().expect("planning conn");
        match name {
            CREATE_TASK => self.create(&conn, &args),
            LIST_TASKS => self.list(&conn),
            CANCEL_TASK => self.cancel(&conn, &args),
            other => ToolOutcome::error(format!("error: no tool named {other}")),
        }
    }
}

fn field<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn blocker_ids(args: &Value) -> Vec<String> {
    args.get("blockers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agent_prompts, agents, endpoints, open_in_memory, tasks, workspaces};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "owner", "Lead", Some("e"), Some("m")).expect("owner");
        agents::create(&conn, "writer", "Writer", Some("e"), Some("m")).expect("writer");
        workspaces::create(&conn, "ws", "Project").expect("ws");
        workspaces::set_owner(&conn, "ws", "owner").expect("own");
        workspaces::add_member(&conn, "writer", "ws").expect("member");
        agent_prompts::set(&conn, "writer", "You write crisp prose.").expect("persona");
        conn
    }

    fn toolbox() -> PlanningToolbox {
        PlanningToolbox::new(seeded(), "ws")
    }

    #[test]
    fn create_task_queues_a_card_assigned_to_a_member() {
        let tb = toolbox();
        let out = tb.call(
            CREATE_TASK,
            r#"{"title":"Draft","spec":"write it","agent":"Writer"}"#,
        );
        assert!(out.ok, "{}", out.result);
        // The card landed in the workspace, assigned to the named member.
        let listed = tb.call(LIST_TASKS, "{}");
        assert!(listed.result.contains("Draft"));
        assert!(listed.result.contains("Writer"));
    }

    #[test]
    fn create_task_defaults_to_the_owner_when_unassigned() {
        let tb = toolbox();
        assert!(tb.call(CREATE_TASK, r#"{"title":"T","spec":"s"}"#).ok);
        let listed = tb.call(LIST_TASKS, "{}");
        assert!(listed.result.contains("→ owner"), "{}", listed.result);
    }

    #[test]
    fn create_task_rejects_a_non_member_assignee() {
        let tb = toolbox();
        let out = tb.call(
            CREATE_TASK,
            r#"{"title":"T","spec":"s","agent":"Stranger"}"#,
        );
        assert!(!out.ok);
        assert!(out.result.contains("not a member"));
    }

    #[test]
    fn create_task_rejects_an_empty_spec() {
        let tb = toolbox();
        let out = tb.call(CREATE_TASK, r#"{"title":"T","spec":"  "}"#);
        assert!(!out.ok);
    }

    #[test]
    fn create_task_with_a_schedule_is_recurrent() {
        let tb = toolbox();
        assert!(
            tb.call(
                CREATE_TASK,
                r#"{"title":"Digest","spec":"summarise","schedule":"0 8 * * *"}"#
            )
            .ok
        );
        let listed = tb.call(LIST_TASKS, "{}");
        assert!(listed.result.contains("Digest"));
    }

    #[test]
    fn cancel_task_only_within_the_workspace() {
        let conn = seeded();
        // A task in a *different* workspace must not be cancellable here.
        workspaces::create(&conn, "other", "Other").expect("other");
        tasks::create(
            &conn,
            &NewTask {
                id: "x1",
                workspace_id: "other",
                agent: None,
                title: "foreign",
                spec: "s",
                schedule: None,
                origin_chat: None,
            },
        )
        .expect("foreign task");
        let tb = PlanningToolbox::new(conn, "ws");
        let out = tb.call(CANCEL_TASK, r#"{"id":"x1"}"#);
        assert!(!out.ok, "must refuse a task from another workspace");
    }

    #[test]
    fn create_task_blocker_must_be_in_this_workspace() {
        let tb = toolbox();
        let out = tb.call(
            CREATE_TASK,
            r#"{"title":"T","spec":"s","blockers":["ghost"]}"#,
        );
        assert!(!out.ok);
        assert!(out.result.contains("blocker"));
    }

    #[test]
    fn defs_advertise_the_three_tools_with_a_member_enum() {
        let tb = toolbox();
        let defs = tb.defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&CREATE_TASK) && names.contains(&LIST_TASKS));
        let create = defs
            .iter()
            .find(|d| d.name == CREATE_TASK)
            .expect("create def");
        let enum_values = &create.parameters["properties"]["agent"]["enum"];
        assert!(
            enum_values.to_string().contains("Writer"),
            "member enum should list Writer: {enum_values}"
        );
    }
}
