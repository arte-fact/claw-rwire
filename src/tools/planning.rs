//! The workspace owner's **planning** toolbox, used during the planning turn a `Command::Ask`
//! spins up: it turns a delegated request into a **proposed** task breakdown (R3 Option A) the
//! user reviews before anything runs. Scoped to one workspace — `propose_tasks` / `list_tasks` /
//! `cancel_task` all act within it, and the assignee enum is exactly that workspace's members
//! (membership is the file-access boundary).
//!
//! Unlike the coder/MCP toolboxes this one owns a `rusqlite` connection (behind a `Mutex`, so
//! the box stays `Sync`): the planning turn runs on a worker thread without the bridge's DB
//! handle, so it reads/writes the board through its own connection (WAL → concurrent with the
//! bridge).

use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::tasks::proposal::{self, ProposalItem};
use crate::db::{agent_prompts, agents, tasks, workspaces};

use super::{ToolDef, ToolOutcome, Toolbox};

pub const PROPOSE_TASKS: &str = "propose_tasks";
pub const LIST_TASKS: &str = "list_tasks";
pub const CANCEL_TASK: &str = "cancel_task";

/// One assignable member of the workspace, with the persona (identity prompt) the model
/// routes by.
struct Member {
    name: String,
    persona: String,
}

/// A planning toolbox bound to one workspace and its own DB connection.
pub struct PlanningToolbox {
    conn: Mutex<Connection>,
    workspace: String,
    /// The plan task whose JSON proposal `propose_tasks` appends to (R3 Option A). Always set while
    /// planning; the user approves the plan to materialise the proposal into real tasks.
    plan_id: Option<String>,
}

impl PlanningToolbox {
    #[must_use]
    pub fn new(conn: Connection, workspace: impl Into<String>, plan_id: Option<String>) -> Self {
        Self {
            conn: Mutex::new(conn),
            workspace: workspace.into(),
            plan_id,
        }
    }

    /// The workspace's members as routing candidates (name + persona snippet).
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
                    name: agent.name,
                    persona,
                })
            })
            .collect()
    }

    fn propose(&self, conn: &Connection, args: &Value) -> ToolOutcome {
        let Some(plan_id) = &self.plan_id else {
            return ToolOutcome::error("error: propose_tasks is only available while planning");
        };
        // Accept either `{tasks:[...]}` (the whole breakdown at once) or a single `{title,spec,…}`.
        let raw: Vec<Value> = match args.get("tasks").and_then(Value::as_array) {
            Some(items) => items.clone(),
            None if args.get("title").is_some() => vec![args.clone()],
            None => return ToolOutcome::error("error: propose_tasks needs a `tasks` array"),
        };
        if raw.is_empty() {
            return ToolOutcome::error("error: propose_tasks needs at least one task");
        }
        let members = self.members(conn);
        let mut items = Vec::with_capacity(raw.len());
        for value in &raw {
            let title = field(value, "title");
            let spec = field(value, "spec");
            if title.is_empty() || spec.is_empty() {
                return ToolOutcome::error(
                    "error: every proposed task needs a non-empty title and spec",
                );
            }
            // An assignee, if named, must be a member; absent → the owner.
            let agent = match value.get("agent").and_then(Value::as_str) {
                Some(name) if !name.trim().is_empty() => {
                    match members.iter().find(|m| m.name == name.trim()) {
                        Some(member) => Some(member.name.clone()),
                        None => {
                            return ToolOutcome::error(format!(
                                "error: {:?} is not a member of this workspace",
                                name.trim()
                            ));
                        }
                    }
                }
                _ => None,
            };
            let schedule = value
                .get("schedule")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            items.push(ProposalItem {
                title: title.trim().to_owned(),
                spec: spec.trim().to_owned(),
                agent,
                schedule,
                blockers: blocker_titles(value),
            });
        }
        let titles: Vec<String> = items.iter().map(|item| item.title.clone()).collect();
        match proposal::append(conn, plan_id, &items) {
            Ok(total) => ToolOutcome::text(format!(
                "proposed {} task(s) ({total} in the plan): {}",
                items.len(),
                titles.join(", ")
            )),
            Err(err) => ToolOutcome::error(format!("error: propose_tasks failed: {err}")),
        }
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
                name: PROPOSE_TASKS.to_owned(),
                description: format!(
                    "Propose the task breakdown for this plan. The user reviews and approves it \
                     before anything runs — call this once with the whole list. {roster_desc}\nUse \
                     `schedule` (a cron expression) for a recurring task, and `blockers` (the \
                     titles of other proposed tasks) for one that must wait on earlier steps."
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "tasks": {
                            "type": "array",
                            "description": "The proposed tasks, in order.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": { "type": "string", "description": "Short card label (also how blockers reference it)." },
                                    "spec": { "type": "string", "description": "The instruction the assignee runs." },
                                    "agent": agent_schema,
                                    "schedule": { "type": "string", "description": "Cron expression for a recurring task; omit for one-shot." },
                                    "blockers": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "Titles of other proposed tasks that must finish first."
                                    }
                                },
                                "required": ["title", "spec"]
                            }
                        }
                    },
                    "required": ["tasks"]
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
            PROPOSE_TASKS => self.propose(&conn, &args),
            LIST_TASKS => self.list(&conn),
            CANCEL_TASK => self.cancel(&conn, &args),
            other => ToolOutcome::error(format!("error: no tool named {other}")),
        }
    }
}

fn field<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn blocker_titles(args: &Value) -> Vec<String> {
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
        tasks::create_plan(&conn, "plan1", "ws", "Plan: x", "do x", None).expect("plan");
        conn
    }

    /// A planning toolbox bound to the seeded plan (proposal mode — always the case now).
    fn toolbox() -> PlanningToolbox {
        PlanningToolbox::new(seeded(), "ws", Some("plan1".to_owned()))
    }

    fn proposal_of(tb: &PlanningToolbox) -> Vec<ProposalItem> {
        let conn = tb.conn.lock().expect("conn");
        proposal::get(&conn, "plan1").expect("proposal")
    }

    #[test]
    fn propose_records_a_task_assigned_to_a_member() {
        let tb = toolbox();
        let out = tb.call(
            PROPOSE_TASKS,
            r#"{"tasks":[{"title":"Draft","spec":"write it","agent":"Writer"}]}"#,
        );
        assert!(out.ok, "{}", out.result);
        let proposal = proposal_of(&tb);
        assert_eq!(proposal.len(), 1);
        assert_eq!(proposal[0].title, "Draft");
        assert_eq!(proposal[0].agent.as_deref(), Some("Writer"));
    }

    #[test]
    fn propose_appends_across_calls_and_keeps_blocker_titles() {
        let tb = toolbox();
        assert!(
            tb.call(PROPOSE_TASKS, r#"{"tasks":[{"title":"A","spec":"s"}]}"#)
                .ok
        );
        assert!(
            tb.call(
                PROPOSE_TASKS,
                r#"{"tasks":[{"title":"B","spec":"s","blockers":["A"]}]}"#
            )
            .ok
        );
        let proposal = proposal_of(&tb);
        assert_eq!(proposal.len(), 2);
        assert_eq!(proposal[1].blockers, vec!["A".to_owned()]);
    }

    #[test]
    fn propose_defaults_to_the_owner_when_unassigned() {
        let tb = toolbox();
        assert!(
            tb.call(PROPOSE_TASKS, r#"{"tasks":[{"title":"T","spec":"s"}]}"#)
                .ok
        );
        assert_eq!(proposal_of(&tb)[0].agent, None);
    }

    #[test]
    fn propose_rejects_a_non_member_assignee() {
        let tb = toolbox();
        let out = tb.call(
            PROPOSE_TASKS,
            r#"{"tasks":[{"title":"T","spec":"s","agent":"Stranger"}]}"#,
        );
        assert!(!out.ok);
        assert!(out.result.contains("not a member"));
    }

    #[test]
    fn propose_rejects_an_empty_spec() {
        let tb = toolbox();
        let out = tb.call(PROPOSE_TASKS, r#"{"tasks":[{"title":"T","spec":"  "}]}"#);
        assert!(!out.ok);
    }

    #[test]
    fn propose_carries_a_schedule() {
        let tb = toolbox();
        assert!(
            tb.call(
                PROPOSE_TASKS,
                r#"{"tasks":[{"title":"Digest","spec":"summarise","schedule":"0 8 * * *"}]}"#
            )
            .ok
        );
        assert_eq!(proposal_of(&tb)[0].schedule.as_deref(), Some("0 8 * * *"));
    }

    #[test]
    fn cancel_task_only_within_the_workspace() {
        let conn = seeded();
        // A task in a *different* workspace must not be cancellable here.
        workspaces::create(&conn, "other", "Other").expect("other");
        tasks::create(
            &conn,
            &tasks::NewTask {
                id: "x1",
                workspace_id: "other",
                agent: None,
                title: "foreign",
                spec: "s",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("foreign task");
        let tb = PlanningToolbox::new(conn, "ws", Some("plan1".to_owned()));
        let out = tb.call(CANCEL_TASK, r#"{"id":"x1"}"#);
        assert!(!out.ok, "must refuse a task from another workspace");
    }

    #[test]
    fn defs_advertise_propose_with_a_member_enum() {
        let tb = toolbox();
        let defs = tb.defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&PROPOSE_TASKS) && names.contains(&LIST_TASKS));
        let propose = defs
            .iter()
            .find(|d| d.name == PROPOSE_TASKS)
            .expect("propose def");
        let enum_values =
            &propose.parameters["properties"]["tasks"]["items"]["properties"]["agent"]["enum"];
        assert!(
            enum_values.to_string().contains("Writer"),
            "member enum should list Writer: {enum_values}"
        );
    }
}
