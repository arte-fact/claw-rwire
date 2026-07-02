//! The agent-thread projection (R2): an agent's working life at **conversation grain**. Each
//! item is a turn — the task being opened (author "you", body = the spec), one of the agent's
//! own runs (its reply prose, phase tag, verdict chip), or an incoming reviewer's verdict +
//! feedback. Tool work is carried only as a count (the page links to the task Peek for the
//! forensics), so the projection stays tiny: no transcripts in shared state.
//!
//! Strictly chronological across the agent's lane (tasks it's the effective assignee of) *and*
//! its out-of-lane runs (reviews of other agents' tasks — capped in SQL). The merged list is
//! windowed to the newest [`TURN_WINDOW`] turns; parked tasks' review gates sit at the tail,
//! like the pending question at the bottom of a chat. The per-role page shape falls out of the
//! data: a producer's thread is its lane as a conversation, a validator's is its review activity.
//!
//! `load` opens a thread (route navigation); `refresh_if_open` re-projects after task lifecycle
//! events, building **outside** the shared-state lock (the open agent is mirrored bridge-side).

use std::sync::{Mutex, OnceLock};

use rusqlite::Connection;
use rwire::{ChangeSet, SharedServerState};

use crate::db::task_runs::TaskRun;
use crate::db::tasks::TaskStatus;
use crate::db::{agents, pipeline, task_runs, tasks, workspaces};
use crate::state::{AgentLive, AgentThread, AgentThreadItem, AgentTurn, App, TaskLink};

use super::tasks::agent_names;

/// The newest turns kept in the projection; older ones fold away (R3 replaces the plain count
/// with the memory fold line).
const TURN_WINDOW: usize = 50;

/// The SQL cap on out-of-lane runs considered for the merge (each is at most one turn).
const RECENT_RUNS: usize = 50;

/// The open (agent, workspace) pair, mirrored bridge-side so `refresh_if_open` can build the
/// projection without holding the shared-state lock. Written by `load`/`close` (bridge thread).
static OPEN_AGENT: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();

fn open_agent_slot() -> &'static Mutex<Option<(String, String)>> {
    OPEN_AGENT.get_or_init(|| Mutex::new(None))
}

fn set_open_agent(pair: Option<(String, String)>) {
    *open_agent_slot().lock().unwrap_or_else(|e| e.into_inner()) = pair;
}

fn open_agent() -> Option<(String, String)> {
    open_agent_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// The current step label for a running task's live entry — the pipeline step name at the task's
/// position, with the loop iteration appended past the first (matching the Peek live entry's tag).
fn step_tag(conn: &Connection, workspace: &str, step_position: i64, iteration: i64) -> String {
    let steps = pipeline::steps_for(conn, workspace).unwrap_or_default();
    let mut tag = steps
        .get(step_position.max(0) as usize)
        .map(|step| step.name.clone())
        .unwrap_or_else(|| "Running".to_owned());
    if iteration > 0 {
        tag.push_str(&format!(" · iter {}", iteration + 1));
    }
    tag
}

/// How many tool calls a run's persisted transcript records (0 for none) — the only thing the
/// thread keeps of the forensics.
fn tool_count(transcript: Option<&str>) -> usize {
    transcript
        .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(json).ok())
        .map_or(0, |calls| calls.len())
}

/// `2026-06-22T08:00:…Z` → `08:00`.
fn hh_mm(stamp: &str) -> String {
    stamp.get(11..16).unwrap_or_default().to_owned()
}

/// One run as a conversational turn. `own` marks the thread agent's replies; a run by anyone
/// else (a reviewer, or "you" for a human verdict) renders as an incoming turn. A human
/// verdict's substance is its feedback, so that becomes the body rather than the "(you) …" stub.
fn run_turn(
    names: &[(String, String)],
    agent_id: &str,
    run: &TaskRun,
    task: TaskLink,
    link_task: bool,
) -> AgentTurn {
    let own = run.agent.as_deref() == Some(agent_id);
    let author = run.agent.as_ref().map_or_else(
        || "you".to_owned(),
        |id| {
            names
                .iter()
                .find(|(i, _)| i == id)
                .map_or_else(|| id.clone(), |(_, n)| n.clone())
        },
    );
    let output = run.output.as_deref().unwrap_or("").trim().to_owned();
    let feedback = run
        .feedback
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_owned);
    // A human verdict records a "(you) pass" stub as output — the feedback is the real
    // message; without feedback, a plain verb (the author + chip already say the rest —
    // rendering the raw "(you) pass" stub read as noise in the live audit).
    let (body, feedback) = if run.agent.is_none() && feedback.is_some() {
        (feedback.clone().unwrap_or_default(), None)
    } else if run.agent.is_none() && run.verdict.is_some() {
        (
            match run.verdict.as_deref() {
                Some("pass") => "Approved.".to_owned(),
                Some("reject") => "Rejected.".to_owned(),
                _ => "Requested changes.".to_owned(),
            },
            None,
        )
    } else if output.is_empty() {
        ("(no output)".to_owned(), feedback)
    } else {
        (output, feedback)
    };
    AgentTurn {
        author,
        own,
        time: hh_mm(&run.created_at),
        tag: crate::state::role_phase(&run.role, run.iteration),
        body,
        feedback,
        verdict: run.verdict.clone(),
        tool_count: tool_count(run.transcript.as_deref()),
        ok: run.ok,
        task,
        link_task,
    }
}

/// Build an agent's thread **in one workspace**: the chronological merge of its lane there
/// (task-opened turns + every run on those tasks) and its out-of-lane runs in that workspace,
/// windowed to the newest `window` turns, with review gates for parked lane tasks at the tail.
/// `None` if the agent or workspace id doesn't resolve.
fn build_with_window(
    conn: &Connection,
    agent_id: &str,
    workspace_id: &str,
    window: usize,
) -> Option<AgentThread> {
    let agent = agents::get(conn, agent_id).ok().flatten()?;
    let workspace = workspaces::get(conn, workspace_id).ok().flatten()?;
    let names = agent_names(conn);
    let lane: Vec<tasks::Task> = tasks::list_for_agent(conn, agent_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|task| task.workspace_id == workspace_id)
        .collect();
    // (sort stamp, item) — ISO-8601 UTC stamps sort lexicographically.
    let mut stamped: Vec<(String, AgentThreadItem)> = Vec::new();
    let mut gates: Vec<AgentThreadItem> = Vec::new();
    let mut live: Vec<AgentLive> = Vec::new();
    for task in &lane {
        let link = TaskLink {
            id: task.id.clone(),
            workspace_id: task.workspace_id.clone(),
            title: task.title.clone(),
        };
        stamped.push((
            task.created_at.clone(),
            turn_item(AgentTurn {
                author: "you".to_owned(),
                own: false,
                time: hh_mm(&task.created_at),
                tag: "task".to_owned(),
                body: task.spec.clone(),
                feedback: None,
                verdict: None,
                tool_count: 0,
                ok: true,
                task: link.clone(),
                link_task: true,
            }),
        ));
        for run in task_runs::list_for_task(conn, &task.id).unwrap_or_default() {
            stamped.push((
                run.created_at.clone(),
                turn_item(run_turn(&names, agent_id, &run, link.clone(), false)),
            ));
        }
        if task.status == TaskStatus::Awaiting {
            gates.push(AgentThreadItem::ReviewGate {
                task_id: task.id.clone(),
            });
        }
        if task.status == TaskStatus::Running {
            live.push(AgentLive {
                task_id: task.id.clone(),
                title: task.title.clone(),
                tag: step_tag(conn, &task.workspace_id, task.step_position, task.iteration),
            });
        }
    }
    // Out-of-lane runs (reviews of other agents' tasks) in this workspace — lane runs are
    // already covered above.
    for review in task_runs::recent_by_agent(conn, agent_id, RECENT_RUNS).unwrap_or_default() {
        if review.workspace_id != workspace_id || lane.iter().any(|task| task.id == review.task_id)
        {
            continue;
        }
        let link = TaskLink {
            id: review.task_id,
            workspace_id: review.workspace_id,
            title: review.task_title,
        };
        stamped.push((
            review.run.created_at.clone(),
            turn_item(run_turn(&names, agent_id, &review.run, link, true)),
        ));
    }
    // Memory events (R3.5 M2): folds/edits merged in at the moment they happened — one muted
    // line each, so history becoming memory is part of the conversation.
    for event in crate::db::agent_memory_log::recent(conn, agent_id, Some(workspace_id), window)
        .unwrap_or_default()
    {
        stamped.push((
            event.created_at.clone(),
            AgentThreadItem::Memory {
                time: hh_mm(&event.created_at),
                note: event.note,
            },
        ));
    }
    stamped.sort_by(|a, b| a.0.cmp(&b.0));
    let truncated = stamped.len().saturating_sub(window);
    let mut items: Vec<AgentThreadItem> = stamped
        .into_iter()
        .skip(truncated)
        .map(|(_, item)| item)
        .collect();
    items.append(&mut gates);
    // The thread's memory row — the fold line's target, shown/editable at the top of the page.
    let memory =
        crate::db::agent_memory::get(conn, agent_id, Some(workspace_id)).unwrap_or_default();
    Some(AgentThread {
        agent_id: agent.id,
        agent_name: agent.name,
        workspace_id: workspace.id,
        workspace_name: workspace.name,
        items,
        live,
        truncated,
        memory: memory.summary,
        memory_paused: memory.paused,
    })
}

fn turn_item(turn: AgentTurn) -> AgentThreadItem {
    AgentThreadItem::Turn(Box::new(turn))
}

fn build(conn: &Connection, agent_id: &str, workspace_id: &str) -> Option<AgentThread> {
    build_with_window(conn, agent_id, workspace_id, TURN_WINDOW)
}

/// Open (or re-open) an agent's workspace thread — set as the open pair and project its
/// timeline. Called when a `/w/:wid/agent/:aid` route resolves.
pub fn load(conn: &Connection, shared: &SharedServerState, agent_id: &str, workspace_id: &str) {
    set_open_agent(Some((agent_id.to_owned(), workspace_id.to_owned())));
    let thread = build(conn, agent_id, workspace_id);
    let agent_id = agent_id.to_owned();
    let workspace_id = workspace_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_OPEN_AGENT,
            App::FIELD_OPEN_AGENT_WORKSPACE,
            App::FIELD_AGENT_THREAD,
        ]),
        move |app| {
            app.open_agent = Some(agent_id);
            app.open_agent_workspace = Some(workspace_id);
            app.agent_thread = thread;
        },
    );
}

/// Close the agent page — clear the open pair + the projection so the nav highlight (and the
/// lifecycle refresh guard) follow the route off the agent pages.
pub fn close(shared: &SharedServerState) {
    set_open_agent(None);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_OPEN_AGENT,
            App::FIELD_OPEN_AGENT_WORKSPACE,
            App::FIELD_AGENT_THREAD,
        ]),
        |app| {
            app.open_agent = None;
            app.open_agent_workspace = None;
            app.agent_thread = None;
        },
    );
}

/// Re-project the open agent thread after a task lifecycle change, so a run finishing / a task
/// advancing updates the page live. A cheap no-op when no agent page is open; the projection is
/// built **before** taking the shared-state lock (the DB work never blocks renders).
pub fn refresh_if_open(conn: &Connection, shared: &SharedServerState) {
    let Some((agent_id, workspace_id)) = open_agent() else {
        return;
    };
    let thread = build(conn, &agent_id, &workspace_id);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_AGENT_THREAD]),
        move |app| {
            if app.open_agent.as_deref() == Some(agent_id.as_str())
                && app.open_agent_workspace.as_deref() == Some(workspace_id.as_str())
            {
                app.agent_thread = thread;
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::task_runs::NewRun;
    use crate::db::tasks::NewTask;
    use crate::db::{endpoints, open_in_memory, workspaces};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "ag-a", "Forge", Some("e"), Some("m")).expect("a");
        agents::create(&conn, "ag-c", "Critic", Some("e"), Some("m")).expect("c");
        workspaces::create(&conn, "ws", "Studio").expect("ws");
        workspaces::set_owner(&conn, "ws", "ag-a").expect("own");
        conn
    }

    fn new_task(id: &'static str, title: &'static str) -> NewTask<'static> {
        NewTask {
            id,
            workspace_id: "ws",
            agent: None,
            title,
            spec: "do it well",
            schedule: None,
            origin_chat: None,
            max_retries: None,
        }
    }

    /// A run with everything defaulted — call sites override via struct-update syntax.
    fn base_run(id: &'static str, task_id: &'static str) -> NewRun<'static> {
        NewRun {
            id,
            task_id,
            position: 0,
            role: "producer",
            agent: None,
            output: None,
            verdict: None,
            feedback: None,
            iteration: 0,
            ok: true,
            transcript: None,
            attempt: 0,
            memory: None,
        }
    }

    fn turns(thread: &AgentThread) -> Vec<&AgentTurn> {
        thread
            .items
            .iter()
            .filter_map(|item| match item {
                AgentThreadItem::Turn(turn) => Some(turn.as_ref()),
                AgentThreadItem::ReviewGate { .. } | AgentThreadItem::Memory { .. } => None,
            })
            .collect()
    }

    #[test]
    fn memory_events_merge_into_the_timeline_scoped_to_the_workspace() {
        let conn = seeded();
        tasks::create(&conn, &new_task("t1", "Build")).expect("t1");
        crate::db::agent_memory_log::append(
            &conn,
            "ag-a",
            Some("ws"),
            crate::db::agent_memory_log::MemoryEventKind::Fold,
            Some("t0"),
            "condensed “Earlier task” into memory",
            "knows things",
        )
        .expect("journal");
        // Another scope's event must not leak into this thread.
        crate::db::agent_memory_log::append(
            &conn,
            "ag-a",
            Some("elsewhere"),
            crate::db::agent_memory_log::MemoryEventKind::Fold,
            None,
            "other workspace fold",
            "",
        )
        .expect("journal other");
        let thread = build(&conn, "ag-a", "ws").expect("thread");
        let notes: Vec<&str> = thread
            .items
            .iter()
            .filter_map(|item| match item {
                AgentThreadItem::Memory { note, .. } => Some(note.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(notes, vec!["condensed “Earlier task” into memory"]);
        // The fold and the task-opened turn share the same clock, so both are present and the
        // memory line carries a rendered time.
        assert_eq!(turns(&thread).len(), 1);
        let has_time = thread
            .items
            .iter()
            .any(|item| matches!(item, AgentThreadItem::Memory { time, .. } if !time.is_empty()));
        assert!(has_time);
    }

    #[test]
    fn build_projects_identity_and_returns_none_for_unknown_agent() {
        let conn = seeded();
        let thread = build(&conn, "ag-a", "ws").expect("thread");
        assert_eq!(thread.agent_id, "ag-a");
        assert_eq!(thread.agent_name, "Forge");
        assert!(build(&conn, "nope", "ws").is_none(), "unknown agent → None");
    }

    #[test]
    fn lane_reads_as_a_conversation_spec_reply_feedback() {
        let conn = seeded();
        tasks::create(&conn, &new_task("t1", "Build a highlighter")).expect("create");
        let transcript = r#"[{"name":"write","arguments":"{}","result":"ok","ok":true},
            {"name":"bash","arguments":"{}","result":"tests pass","ok":true}]"#;
        task_runs::append(
            &conn,
            &NewRun {
                agent: Some("ag-a"),
                output: Some("built the first cut"),
                transcript: Some(transcript),
                ..base_run("r1", "t1")
            },
        )
        .expect("r1");
        task_runs::append(
            &conn,
            &NewRun {
                role: "validator",
                agent: Some("ag-c"),
                output: Some("solid, one edge case"),
                verdict: Some("revise"),
                feedback: Some("handle empty hex"),
                ..base_run("r2", "t1")
            },
        )
        .expect("r2");

        let thread = build(&conn, "ag-a", "ws").expect("thread");
        let turns = turns(&thread);
        assert_eq!(turns.len(), 3);
        // you: the spec, linked to the task.
        assert_eq!(turns[0].author, "you");
        assert_eq!(turns[0].tag, "task");
        assert_eq!(turns[0].body, "do it well");
        assert!(turns[0].link_task);
        assert_eq!(turns[0].task.title, "Build a highlighter");
        // forge: its reply prose — tool work as a count, not transcripts.
        assert!(turns[1].own);
        assert_eq!(turns[1].author, "Forge");
        assert_eq!(turns[1].body, "built the first cut");
        assert_eq!(turns[1].tool_count, 2);
        assert_eq!(turns[1].tag, "Produce");
        // critic: incoming review with verdict + feedback.
        assert!(!turns[2].own);
        assert_eq!(turns[2].author, "Critic");
        assert_eq!(turns[2].verdict.as_deref(), Some("revise"));
        assert_eq!(turns[2].feedback.as_deref(), Some("handle empty hex"));
    }

    #[test]
    fn human_verdict_reads_as_your_feedback_turn() {
        let conn = seeded();
        tasks::create(&conn, &new_task("t1", "Task")).expect("create");
        task_runs::append(
            &conn,
            &NewRun {
                role: "validator",
                output: Some("(you) revise"),
                verdict: Some("revise"),
                feedback: Some("make the header blue"),
                ..base_run("r1", "t1")
            },
        )
        .expect("r1");

        let thread = build(&conn, "ag-a", "ws").expect("thread");
        let turn = turns(&thread)[1];
        assert_eq!(turn.author, "you");
        assert!(!turn.own);
        // The feedback IS the message — not the "(you) revise" stub.
        assert_eq!(turn.body, "make the header blue");
        assert_eq!(turn.feedback, None);
        assert_eq!(turn.verdict.as_deref(), Some("revise"));
    }

    #[test]
    fn human_verdict_without_feedback_reads_as_a_plain_verb() {
        let conn = seeded();
        tasks::create(&conn, &new_task("t1", "Task")).expect("create");
        task_runs::append(
            &conn,
            &NewRun {
                role: "validator",
                output: Some("(you) pass"),
                verdict: Some("pass"),
                ..base_run("r1", "t1")
            },
        )
        .expect("r1");
        let thread = build(&conn, "ag-a", "ws").expect("thread");
        let turn = turns(&thread)[1];
        // Author + chip already say who and what — the raw "(you) pass" stub read as noise.
        assert_eq!(turn.body, "Approved.");
        assert_eq!(turn.verdict.as_deref(), Some("pass"));
    }

    #[test]
    fn out_of_lane_reviews_appear_as_own_linked_turns() {
        let conn = seeded();
        // A task in Forge's lane; Critic reviews it. Critic has no lane of its own.
        tasks::create(&conn, &new_task("t1", "Forge's task")).expect("create");
        task_runs::append(
            &conn,
            &NewRun {
                role: "validator",
                agent: Some("ag-c"),
                output: Some("looks good"),
                verdict: Some("pass"),
                ..base_run("r1", "t1")
            },
        )
        .expect("r1");

        let thread = build(&conn, "ag-c", "ws").expect("critic thread");
        let critic_turns = turns(&thread);
        // No task-opened turn (not Critic's lane) — just its review activity, linked.
        assert_eq!(critic_turns.len(), 1);
        assert!(critic_turns[0].own);
        assert_eq!(critic_turns[0].author, "Critic");
        assert!(critic_turns[0].link_task);
        assert_eq!(critic_turns[0].task.title, "Forge's task");
        assert_eq!(critic_turns[0].verdict.as_deref(), Some("pass"));

        // And Forge's thread carries the same run as an *incoming* turn exactly once.
        let forge = build(&conn, "ag-a", "ws").expect("forge thread");
        let incoming: Vec<_> = turns(&forge)
            .into_iter()
            .filter(|t| t.author == "Critic")
            .collect();
        assert_eq!(incoming.len(), 1, "no duplicate from the by-agent query");
    }

    #[test]
    fn window_keeps_the_newest_turns_and_counts_the_folded() {
        let conn = seeded();
        tasks::create(&conn, &new_task("t1", "Task")).expect("create");
        for id in ["r1", "r2", "r3"] {
            task_runs::append(
                &conn,
                &NewRun {
                    agent: Some("ag-a"),
                    output: Some(id),
                    ..base_run(id, "t1")
                },
            )
            .expect(id);
        }

        // 4 turns total (opened + 3 runs); window 2 keeps the newest 2.
        let thread = build_with_window(&conn, "ag-a", "ws", 2).expect("thread");
        let turns = turns(&thread);
        assert_eq!(thread.truncated, 2);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].body, "r2");
        assert_eq!(turns[1].body, "r3");
    }

    #[test]
    fn gates_sit_at_the_tail_and_running_tasks_go_live() {
        let conn = seeded();
        tasks::create(&conn, &new_task("t1", "Parked")).expect("t1");
        tasks::create(&conn, &new_task("t2", "Running")).expect("t2");
        tasks::set_status(&conn, "t1", TaskStatus::Awaiting).expect("park");
        tasks::set_status(&conn, "t2", TaskStatus::Running).expect("run");

        let thread = build(&conn, "ag-a", "ws").expect("thread");
        // The gate is the last item, after every turn.
        assert!(
            matches!(
                thread.items.last(),
                Some(AgentThreadItem::ReviewGate { task_id }) if task_id == "t1"
            ),
            "the pending question sits at the bottom"
        );
        assert_eq!(thread.live.len(), 1);
        assert_eq!(thread.live[0].task_id, "t2");
        assert_eq!(thread.live[0].title, "Running");
    }
}
