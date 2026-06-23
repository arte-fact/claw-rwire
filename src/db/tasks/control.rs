//! User-driven run control over a task (stop / start / restart) and the engine's auto-retry
//! bookkeeping. Split out of `tasks/mod.rs` (storage + the scheduler scan) so the lifecycle
//! mutations have room to grow. All operate on the `tasks` row directly.
//!
//! The **dispatch guard**: `run_epoch` is bumped on every launch and on every stop / cancel /
//! restart. A worker captures the epoch it launched at; the scheduler drops its result unless
//! the captured epoch still matches the row — so a stopped/cancelled/restarted task never has a
//! superseded run land on it.

use rusqlite::{Connection, params};

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// Bump the dispatch guard and return its new value — called at every launch so the worker
/// carries a fresh epoch, and by the abandon paths (stop / cancel / restart) so an in-flight
/// run's result is dropped on arrival.
pub fn bump_epoch(conn: &Connection, id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!(
            "UPDATE tasks SET run_epoch = run_epoch + 1, updated_at = {NOW}
              WHERE id = ?1 RETURNING run_epoch"
        ),
        params![id],
        |row| row.get(0),
    )
}

/// Stop (pause) a task: park it `stopped` — idle, lane-free, editable — and bump `run_epoch` so
/// an in-flight run is abandoned. Works from `pending` / `running` / `awaiting`.
pub fn stop(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "UPDATE tasks SET status = 'stopped', run_epoch = run_epoch + 1, updated_at = {NOW}
              WHERE id = ?1"
        ),
        params![id],
    )?;
    Ok(())
}

/// Resume a stopped task: re-queue it (`pending`) and clear any retry-backoff hold so the next
/// tick can pick it up. Keeps `step_position` and the run chain — it continues where it paused.
pub fn resume(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "UPDATE tasks SET status = 'pending', next_run = NULL, updated_at = {NOW}
              WHERE id = ?1"
        ),
        params![id],
    )?;
    Ok(())
}

/// Restart a task from a clean slate: back to `pending` at step 0, iteration 0, retry budget
/// reset, a fresh `attempt` (so the run chain's earlier attempts aren't fed back as context),
/// result/next_run cleared, and the epoch bumped (drop any in-flight run).
pub fn restart(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "UPDATE tasks
                SET status = 'pending', step_position = 0, iteration = 0, auto_retries = 0,
                    attempt = attempt + 1, result = NULL, next_run = NULL,
                    run_epoch = run_epoch + 1, updated_at = {NOW}
              WHERE id = ?1"
        ),
        params![id],
    )?;
    Ok(())
}

/// Record an auto-retry after a failed run: back to `pending`, consume one of the retry budget,
/// and hold it until `next_run` (the backoff). Keeps `step_position` and the run chain, so the
/// retry re-runs the same step with its prior (successful) context.
pub fn note_retry(conn: &Connection, id: &str, next_run: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "UPDATE tasks
                SET status = 'pending', auto_retries = auto_retries + 1, next_run = ?2,
                    updated_at = {NOW}
              WHERE id = ?1"
        ),
        params![id, next_run],
    )?;
    Ok(())
}

/// Approve a plan (R3): release its held children — `stopped` → `pending` — so the scheduler
/// picks them up.
pub fn release_plan(conn: &Connection, plan_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "UPDATE tasks SET status = 'pending', updated_at = {NOW}
              WHERE plan_id = ?1 AND status = 'stopped'"
        ),
        params![plan_id],
    )?;
    Ok(())
}

/// Reject a plan (R3): cancel its held/queued children so nothing runs.
pub fn cancel_plan_children(conn: &Connection, plan_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "UPDATE tasks SET status = 'cancelled', run_epoch = run_epoch + 1, updated_at = {NOW}
              WHERE plan_id = ?1 AND status IN ('stopped', 'pending')"
        ),
        params![plan_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{NewTask, TaskStatus, create, get, next_ready, set_status};
    use crate::db::{endpoints, open_in_memory, workspaces};
    use rusqlite::Connection;

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        workspaces::create(&conn, "ws", "Project").expect("ws");
        create(
            &conn,
            &NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "t1",
                spec: "do the thing",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("create");
        conn
    }

    #[test]
    fn bump_epoch_returns_monotonic_values() {
        let conn = seeded();
        assert_eq!(super::bump_epoch(&conn, "t1").expect("bump"), 1);
        assert_eq!(super::bump_epoch(&conn, "t1").expect("bump"), 2);
        assert_eq!(get(&conn, "t1").expect("get").unwrap().run_epoch, 2);
    }

    #[test]
    fn stop_parks_and_bumps_epoch() {
        let conn = seeded();
        set_status(&conn, "t1", TaskStatus::Running).expect("run");
        super::stop(&conn, "t1").expect("stop");
        let task = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(task.status, TaskStatus::Stopped);
        assert_eq!(
            task.run_epoch, 1,
            "a stopped run is abandoned via the epoch"
        );
    }

    #[test]
    fn stopped_is_invisible_to_next_ready() {
        let conn = seeded();
        super::stop(&conn, "t1").expect("stop");
        assert!(
            next_ready(&conn, "ws").expect("scan").is_none(),
            "the ready-scan only picks pending tasks"
        );
    }

    #[test]
    fn resume_requeues_and_clears_next_run() {
        let conn = seeded();
        super::note_retry(&conn, "t1", "2999-01-01T00:00:00.000Z").expect("retry hold");
        super::resume(&conn, "t1").expect("resume");
        let task = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.next_run, None, "the backoff hold is cleared on resume");
    }

    #[test]
    fn restart_resets_step_iteration_retries_and_bumps_attempt() {
        let conn = seeded();
        // Advance the task's run state, then restart it.
        super::super::set_step(&conn, "t1", 3).expect("step");
        super::super::set_iteration(&conn, "t1", 2).expect("iter");
        super::note_retry(&conn, "t1", "2999-01-01T00:00:00.000Z").expect("retry");
        super::super::finish(&conn, "t1", TaskStatus::Failed, Some("boom")).expect("fail");

        super::restart(&conn, "t1").expect("restart");
        let task = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.step_position, 0);
        assert_eq!(task.iteration, 0);
        assert_eq!(task.auto_retries, 0);
        assert_eq!(task.attempt, 1, "a fresh attempt isolates the context");
        assert_eq!(task.result, None);
        assert_eq!(task.next_run, None);
    }

    #[test]
    fn note_retry_increments_and_holds_via_next_run() {
        let conn = seeded();
        super::note_retry(&conn, "t1", "2999-01-01T00:00:00.000Z").expect("retry");
        let task = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.auto_retries, 1);
        assert_eq!(task.next_run.as_deref(), Some("2999-01-01T00:00:00.000Z"));
        // A future next_run keeps it out of the ready-scan until the backoff elapses.
        assert!(next_ready(&conn, "ws").expect("scan").is_none());
    }

    #[test]
    fn cancel_bumps_epoch_so_a_late_result_is_dropped() {
        let conn = seeded();
        set_status(&conn, "t1", TaskStatus::Running).expect("run");
        super::super::cancel(&conn, "t1").expect("cancel");
        let task = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert_eq!(task.run_epoch, 1);
    }

    fn child<'a>(id: &'a str) -> NewTask<'a> {
        NewTask {
            id,
            workspace_id: "ws",
            agent: None,
            title: id,
            spec: "s",
            schedule: None,
            origin_chat: None,
            max_retries: None,
        }
    }

    #[test]
    fn held_children_are_stopped_invisible_then_released_to_pending() {
        let conn = seeded();
        super::super::create_plan(&conn, "plan1", "ws", "Plan: x", "do x", None).expect("plan");
        super::super::create_child(&conn, &child("c1"), "plan1").expect("c1");
        super::super::create_child(&conn, &child("c2"), "plan1").expect("c2");
        // Both children are held (stopped) and tagged with the plan.
        let c1 = get(&conn, "c1").expect("get").unwrap();
        assert_eq!(c1.status, TaskStatus::Stopped);
        assert_eq!(c1.plan_id.as_deref(), Some("plan1"));
        assert_eq!(
            super::super::children_of_plan(&conn, "plan1")
                .expect("kids")
                .len(),
            2
        );
        // Approve → released to pending.
        super::release_plan(&conn, "plan1").expect("release");
        assert_eq!(
            get(&conn, "c1").expect("get").unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(
            get(&conn, "c2").expect("get").unwrap().status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn reject_cancels_a_plans_held_children() {
        let conn = seeded();
        super::super::create_plan(&conn, "plan1", "ws", "Plan", "s", None).expect("plan");
        super::super::create_child(&conn, &child("c1"), "plan1").expect("c1");
        super::cancel_plan_children(&conn, "plan1").expect("cancel");
        assert_eq!(
            get(&conn, "c1").expect("get").unwrap().status,
            TaskStatus::Cancelled
        );
    }

    #[test]
    fn next_ready_skips_a_plan_task() {
        let conn = seeded();
        // A plan task forced pending must still be skipped by the scheduler scan.
        super::super::create_plan(&conn, "plan1", "ws", "Plan", "s", None).expect("plan");
        set_status(&conn, "plan1", TaskStatus::Pending).expect("pending");
        // `t1` (seeded, pending) is the only runnable task; the plan is never picked.
        assert_eq!(next_ready(&conn, "ws").expect("scan").unwrap().id, "t1");
    }
}
