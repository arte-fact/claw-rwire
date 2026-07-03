//! The task store's unit tests — types, mutations, and the scan/list queries together, so
//! a schema change breaks loudly in one place.

use super::*;
use crate::db::{agents, endpoints, open_in_memory, workspaces};

fn seeded() -> Connection {
    let conn = open_in_memory().expect("db");
    endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
    agents::create(&conn, "ag-a", "Coder", Some("e"), Some("m")).expect("a");
    workspaces::create(&conn, "ws", "Project").expect("ws");
    conn
}

fn task(id: &str) -> NewTask<'_> {
    NewTask {
        id,
        workspace_id: "ws",
        agent: None,
        title: id,
        spec: "do the thing",
        schedule: None,
        origin_chat: None,
        max_retries: None,
    }
}

#[test]
fn create_get_defaults_to_pending_oneshot() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("create");
    let t = get(&conn, "t1").expect("get").expect("some");
    assert_eq!(t.status, TaskStatus::Pending);
    assert_eq!(t.kind, TaskKind::OneShot);
    assert_eq!(t.workspace_id, "ws");
    assert_eq!(
        t.max_retries, None,
        "no override → the global default applies"
    );
}

/// Record an ok producer run by `agent` on `task_id` (the chain evidence
/// `last_finished_for_agent` matches by).
fn produced(conn: &Connection, run_id: &str, task_id: &str, agent: &str) {
    crate::db::task_runs::append(
        conn,
        &crate::db::task_runs::NewRun {
            id: run_id,
            task_id,
            position: 0,
            role: "producer",
            agent: Some(agent),
            output: Some("did it"),
            verdict: None,
            feedback: None,
            iteration: 0,
            ok: true,
            transcript: None,
            attempt: 0,
            memory: None,
            commits: None,
        },
    )
    .expect("run");
}

#[test]
fn last_finished_for_agent_picks_the_newest_produced_terminal_task() {
    let conn = seeded();
    for id in ["t1", "t2", "t3", "t4"] {
        create(&conn, &task(id)).expect("create");
    }
    // t1: done, produced by ag-a (the expected hit — t2 is newer but by someone else,
    // t3 is still running, t4 is the task being launched and must be excluded).
    produced(&conn, "r1", "t1", "ag-a");
    finish(&conn, "t1", TaskStatus::Done, Some("ok")).expect("finish t1");
    produced(&conn, "r2", "t2", "ag-b");
    finish(&conn, "t2", TaskStatus::Done, Some("ok")).expect("finish t2");
    produced(&conn, "r3", "t3", "ag-a");
    set_status(&conn, "t3", TaskStatus::Running).expect("running t3");
    produced(&conn, "r4", "t4", "ag-a");
    finish(&conn, "t4", TaskStatus::Done, Some("ok")).expect("finish t4");

    let hit = last_finished_for_agent(&conn, "ag-a", "ws", "t4").expect("query");
    assert_eq!(hit.map(|t| t.id), Some("t1".to_owned()));
    // A failed task still counts — what broke is worth remembering.
    finish(&conn, "t3", TaskStatus::Failed, Some("broke")).expect("fail t3");
    let hit = last_finished_for_agent(&conn, "ag-a", "ws", "t4").expect("query");
    assert_eq!(hit.map(|t| t.id), Some("t3".to_owned()));
    // No produced terminal task in another workspace — the wall holds.
    let miss = last_finished_for_agent(&conn, "ag-a", "elsewhere", "t4").expect("query");
    assert!(miss.is_none());
}

#[test]
fn create_persists_a_max_retries_override() {
    let conn = seeded();
    create(
        &conn,
        &NewTask {
            max_retries: Some(5),
            ..task("t1")
        },
    )
    .expect("create");
    assert_eq!(get(&conn, "t1").expect("get").unwrap().max_retries, Some(5));
}

#[test]
fn schedule_makes_it_recurrent() {
    let conn = seeded();
    create(
        &conn,
        &NewTask {
            schedule: Some("0 8 * * *"),
            ..task("t1")
        },
    )
    .expect("create");
    let t = get(&conn, "t1").expect("get").unwrap();
    assert_eq!(t.kind, TaskKind::Recurrent);
    assert_eq!(t.schedule.as_deref(), Some("0 8 * * *"));
}

#[test]
fn cancel_is_terminal() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("create");
    cancel(&conn, "t1").expect("cancel");
    assert_eq!(
        get(&conn, "t1").expect("get").unwrap().status,
        TaskStatus::Cancelled
    );
}

#[test]
fn running_in_reports_the_running_task_only() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("t1");
    assert!(running_in(&conn, "ws").expect("none").is_none());
    set_status(&conn, "t1", TaskStatus::Running).expect("run");
    assert_eq!(running_in(&conn, "ws").expect("some").unwrap().id, "t1");
}

#[test]
fn next_ready_skips_running_and_terminal() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("t1");
    set_status(&conn, "t1", TaskStatus::Running).expect("run");
    // Only task is running → nothing ready.
    assert!(next_ready(&conn, "ws", None).expect("scan").is_none());
    create(&conn, &task("t2")).expect("t2");
    assert_eq!(
        next_ready(&conn, "ws", None).expect("scan").unwrap().id,
        "t2"
    );
}

#[test]
fn next_ready_skips_a_task_with_an_unfinished_blocker() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("t1"); // older, but blocked
    create(&conn, &task("t2")).expect("t2"); // the blocker
    conn.execute(
        "INSERT INTO task_blockers (task_id, blocked_by) VALUES ('t1', 't2')",
        [],
    )
    .expect("block");
    // t1 is older (would sort first) but is blocked by the still-pending t2 → t2 runs.
    assert_eq!(
        next_ready(&conn, "ws", None).expect("scan").unwrap().id,
        "t2"
    );
    finish(&conn, "t2", TaskStatus::Done, Some("ok")).expect("finish");
    // Blocker done → t1 becomes ready.
    assert_eq!(
        next_ready(&conn, "ws", None).expect("scan").unwrap().id,
        "t1"
    );
}

#[test]
fn next_ready_gates_a_recurrent_task_on_its_next_run() {
    let conn = seeded();
    create(
        &conn,
        &NewTask {
            schedule: Some("0 8 * * *"),
            ..task("t1")
        },
    )
    .expect("create");
    // A future next_run hides it from the ready scan…
    reschedule(&conn, "t1", "2999-01-01T00:00:00.000Z", Some("ran")).expect("future");
    assert!(next_ready(&conn, "ws", None).expect("scan").is_none());
    // …a past next_run makes it ready again (its slot has arrived).
    reschedule(&conn, "t1", "2000-01-01T00:00:00.000Z", Some("ran")).expect("past");
    let ready = next_ready(&conn, "ws", None).expect("scan").expect("ready");
    assert_eq!(ready.id, "t1");
    assert_eq!(
        ready.status,
        TaskStatus::Pending,
        "rescheduled back to pending"
    );
}

#[test]
fn update_rewrites_fields_and_recomputes_kind() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("create");
    update(
        &conn,
        "t1",
        "New title",
        "new spec",
        None,
        Some("0 9 * * *"),
        Some(5),
    )
    .expect("update");
    let t = get(&conn, "t1").expect("get").unwrap();
    assert_eq!(t.title, "New title");
    assert_eq!(t.spec, "new spec");
    assert_eq!(t.kind, TaskKind::Recurrent, "a schedule makes it recurrent");
    assert_eq!(t.max_retries, Some(5), "the retry budget round-trips");
    // Clearing the schedule flips it back to one-shot; clearing max_retries restores default.
    update(&conn, "t1", "New title", "new spec", None, None, None).expect("update2");
    let t = get(&conn, "t1").expect("get").unwrap();
    assert_eq!(t.kind, TaskKind::OneShot);
    assert_eq!(t.max_retries, None);
}

#[test]
fn list_for_agent_covers_assigned_and_owned_unassigned_tasks() {
    let conn = seeded();
    agents::create(&conn, "ag-b", "Writer", Some("e"), Some("m")).expect("b");
    workspaces::create(&conn, "ws2", "Other").expect("ws2");
    workspaces::set_owner(&conn, "ws", "ag-a").expect("own ws");
    // Assigned directly to ag-a (in a workspace it doesn't own).
    create(
        &conn,
        &NewTask {
            id: "assigned",
            workspace_id: "ws2",
            agent: Some("ag-a"),
            ..task("assigned")
        },
    )
    .expect("assigned");
    // Unassigned in ag-a's owned workspace → defaults to the owner, so it's ag-a's.
    create(&conn, &task("owned")).expect("owned");
    // Assigned to someone else → not ag-a's.
    create(
        &conn,
        &NewTask {
            id: "other",
            workspace_id: "ws2",
            agent: Some("ag-b"),
            ..task("other")
        },
    )
    .expect("other");

    let ids: Vec<String> = list_for_agent(&conn, "ag-a")
        .expect("list")
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert!(ids.contains(&"assigned".to_owned()));
    assert!(ids.contains(&"owned".to_owned()));
    assert!(
        !ids.contains(&"other".to_owned()),
        "another agent's task is not this agent's"
    );
    // The non-owner ag-b only sees its explicit assignment, not ws's unassigned task.
    let b_ids: Vec<String> = list_for_agent(&conn, "ag-b")
        .expect("list b")
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(b_ids, vec!["other".to_owned()]);
}

#[test]
fn set_blockers_replaces_the_blocker_set() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("t1");
    create(&conn, &task("t2")).expect("t2");
    create(&conn, &task("t3")).expect("t3");
    set_blockers(&conn, "t1", &["t2".to_owned(), "t3".to_owned()]).expect("set");
    let mut blockers = blockers_of(&conn, "t1").expect("of");
    blockers.sort();
    assert_eq!(blockers, vec!["t2".to_owned(), "t3".to_owned()]);
    // Replacing collapses to just t3.
    set_blockers(&conn, "t1", &["t3".to_owned()]).expect("set2");
    assert_eq!(
        blockers_of(&conn, "t1").expect("of2"),
        vec!["t3".to_owned()]
    );
}

#[test]
fn other_running_in_excludes_self_and_finds_peers() {
    let conn = seeded();
    create(&conn, &task("a")).expect("a");
    create(&conn, &task("b")).expect("b");
    set_status(&conn, "a", TaskStatus::Running).expect("run a");
    // From a's own perspective no *other* task is running…
    assert!(
        other_running_in(&conn, "ws", "a", None)
            .expect("scan")
            .is_none()
    );
    // …but from b's perspective, a holds the lane.
    assert_eq!(
        other_running_in(&conn, "ws", "b", None)
            .expect("scan")
            .unwrap()
            .id,
        "a"
    );
}

#[test]
fn reschedule_resets_the_retry_budget_each_cycle() {
    // Regression (B4): a recurrent task that spent its retry budget in one cycle must get a
    // fresh budget on the next, not stay maxed out forever.
    let conn = seeded();
    create(
        &conn,
        &NewTask {
            schedule: Some("0 8 * * *"),
            ..task("t1")
        },
    )
    .expect("create");
    note_retry(&conn, "t1", "2999-01-01T00:00:00.000Z").expect("retry"); // auto_retries → 1
    assert_eq!(get(&conn, "t1").expect("get").unwrap().auto_retries, 1);
    reschedule(&conn, "t1", "2000-01-01T00:00:00.000Z", Some("ran")).expect("reschedule");
    assert_eq!(
        get(&conn, "t1").expect("get").unwrap().auto_retries,
        0,
        "each cron cycle starts with a fresh retry budget"
    );
}

#[test]
fn cancel_dependents_cascades_through_the_chain() {
    // Regression (B2): when a blocker fails, its waiting followers (and theirs) must be
    // cancelled rather than left pending forever.
    let conn = seeded();
    create(&conn, &task("a")).expect("a"); // the blocker that fails
    create(&conn, &task("b")).expect("b"); // blocked by a
    create(&conn, &task("c")).expect("c"); // blocked by b
    add_blocker(&conn, "b", "a").expect("b<-a");
    add_blocker(&conn, "c", "b").expect("c<-b");
    let cancelled = cancel_dependents(&conn, "a", "blocker did not succeed").expect("cascade");
    assert_eq!(cancelled.len(), 2, "both b and c are cancelled");
    for id in ["b", "c"] {
        let t = get(&conn, id).expect("get").unwrap();
        assert_eq!(t.status, TaskStatus::Cancelled);
        assert_eq!(t.result.as_deref(), Some("blocker did not succeed"));
    }
}

#[test]
fn cancel_dependents_leaves_started_followers_untouched() {
    let conn = seeded();
    create(&conn, &task("a")).expect("a");
    create(&conn, &task("b")).expect("b");
    add_blocker(&conn, "b", "a").expect("b<-a");
    set_status(&conn, "b", TaskStatus::Running).expect("run b");
    // A running dependent already cleared its blockers; never cancel it out from under a worker.
    assert!(
        cancel_dependents(&conn, "a", "x")
            .expect("cascade")
            .is_empty()
    );
    assert_eq!(
        get(&conn, "b").expect("get").unwrap().status,
        TaskStatus::Running
    );
}

#[test]
fn finish_sets_terminal_status_and_result() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("t1");
    finish(&conn, "t1", TaskStatus::Done, Some("the report")).expect("finish");
    let t = get(&conn, "t1").expect("get").unwrap();
    assert_eq!(t.status, TaskStatus::Done);
    assert_eq!(t.result.as_deref(), Some("the report"));
}

#[test]
fn list_orders_running_then_pending_then_terminal() {
    let conn = seeded();
    create(&conn, &task("t1")).expect("t1");
    create(&conn, &task("t2")).expect("t2");
    create(&conn, &task("t3")).expect("t3");
    set_status(&conn, "t1", TaskStatus::Done).expect("done");
    set_status(&conn, "t2", TaskStatus::Running).expect("run");
    let ids: Vec<String> = list_for_workspace(&conn, "ws")
        .expect("list")
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, vec!["t2".to_owned(), "t3".to_owned(), "t1".to_owned()]);
}

#[test]
fn count_awaiting_by_workspace_groups_only_parked_tasks() {
    let conn = seeded();
    workspaces::create(&conn, "ws2", "Other").expect("ws2");
    // ws: two parked + one still pending; ws2: one parked; a third workspace has none.
    for id in ["a1", "a2", "pending"] {
        create(&conn, &task(id)).expect("create");
    }
    create(
        &conn,
        &NewTask {
            workspace_id: "ws2",
            ..task("b1")
        },
    )
    .expect("create");
    for id in ["a1", "a2", "b1"] {
        set_status(&conn, id, TaskStatus::Awaiting).expect("park");
    }

    let mut counts = count_awaiting_by_workspace(&conn).expect("counts");
    counts.sort();
    // Only `awaiting` tasks count; the pending one and the empty workspace are absent.
    assert_eq!(counts, vec![("ws".to_owned(), 2), ("ws2".to_owned(), 1)]);
}

#[test]
fn count_inflight_by_agent_resolves_the_effective_assignee() {
    let conn = seeded();
    agents::create(&conn, "ag-b", "Writer", Some("e"), Some("m")).expect("b");
    // ag-a owns ws → an unassigned running task counts against ag-a.
    workspaces::set_owner(&conn, "ws", "ag-a").expect("own");
    create(&conn, &task("owned-run")).expect("owned-run");
    set_status(&conn, "owned-run", TaskStatus::Running).expect("run");
    // ag-b is explicitly assigned an awaiting task.
    create(
        &conn,
        &NewTask {
            id: "b-await",
            agent: Some("ag-b"),
            ..task("b-await")
        },
    )
    .expect("b-await");
    set_status(&conn, "b-await", TaskStatus::Awaiting).expect("park");
    // A pending task is not in-flight → excluded.
    create(&conn, &task("pending")).expect("pending");

    let mut rows = count_inflight_by_agent(&conn).expect("counts");
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("ag-a".to_owned(), 1, 0), // owner's unassigned running task
            ("ag-b".to_owned(), 0, 1), // assignee's awaiting task
        ]
    );
}
