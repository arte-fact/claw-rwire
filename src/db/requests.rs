//! Requests (G1 of `docs/requests.md`): the first-class unit above tasks — one user ask,
//! from delegation to delivery. A Request owns its plan and tasks (`tasks.request_id`), maps
//! onto one branch name across every repo it touches (`request_repos` carries the per-repo
//! branch point and merge commit), and each **active** request — the oldest [`MAX_ACTIVE`]
//! executing/validating ones — holds its own scheduler lane and worktree (G4); requests
//! beyond the cap queue FIFO and activate as earlier ones deliver.

use rusqlite::{Connection, OptionalExtension, params};

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// The request lifecycle. `Planning` covers ask → plan-review (the plan task's own status
/// distinguishes the gate); `Executing` starts at plan approval — the moment a branch can be
/// cut; the terminal states end the lane's claim on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    Planning,
    Executing,
    /// Every task landed; the branch diff awaits its macro review (agent or human, G3).
    Validating,
    Delivered,
    Rejected,
    Abandoned,
    /// Delivered, then reverted on `develop` (`git revert -m1 <merge>`, G3).
    RolledBack,
}

impl RequestStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Executing => "executing",
            Self::Validating => "validating",
            Self::Delivered => "delivered",
            Self::Rejected => "rejected",
            Self::Abandoned => "abandoned",
            Self::RolledBack => "rolled-back",
        }
    }

    #[must_use]
    pub fn from_str(value: &str) -> Self {
        match value {
            "executing" => Self::Executing,
            "validating" => Self::Validating,
            "delivered" => Self::Delivered,
            "rejected" => Self::Rejected,
            "abandoned" => Self::Abandoned,
            "rolled-back" => Self::RolledBack,
            _ => Self::Planning,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: String,
    pub workspace_id: String,
    pub origin_chat: Option<String>,
    pub title: String,
    /// The user's words, verbatim — the request page shows them and G3's validation judges
    /// the branch diff against them.
    pub ask: String,
    pub status: RequestStatus,
    /// `req/<short-id>-<slug>`, minted at plan approval — `None` while planning.
    pub branch: Option<String>,
    /// The macro-validation verdict + feedback (G3), or `None` before validation ran.
    pub validation: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One repo a request touched: the branch point recorded when the branch was cut there,
/// and the merge commit once delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRepo {
    pub repo: String,
    pub branch: String,
    pub base_commit: String,
    pub merge_commit: Option<String>,
}

const SELECT: &str = "SELECT id, workspace_id, origin_chat_id, title, ask, status, branch, \
                      validation, created_at, updated_at FROM requests";

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Request> {
    Ok(Request {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        origin_chat: row.get(2)?,
        title: row.get(3)?,
        ask: row.get(4)?,
        status: RequestStatus::from_str(&row.get::<_, String>(5)?),
        branch: row.get(6)?,
        validation: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// Create a request in `planning` — the ask lands here before its plan task exists.
pub fn create(
    conn: &Connection,
    id: &str,
    workspace_id: &str,
    origin_chat: Option<&str>,
    title: &str,
    ask: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO requests (id, workspace_id, origin_chat_id, title, ask, status,
                                   created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'planning', {NOW}, {NOW})"
        ),
        params![id, workspace_id, origin_chat, title, ask],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Request>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), params![id], from_row)
        .optional()
}

pub fn set_status(conn: &Connection, id: &str, status: RequestStatus) -> rusqlite::Result<()> {
    conn.execute(
        &format!("UPDATE requests SET status = ?2, updated_at = {NOW} WHERE id = ?1"),
        params![id, status.as_str()],
    )?;
    Ok(())
}

/// Record the macro-validation outcome (G3): the verdict + feedback shown on the page.
pub fn set_validation(conn: &Connection, id: &str, note: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("UPDATE requests SET validation = ?2, updated_at = {NOW} WHERE id = ?1"),
        params![id, note],
    )?;
    Ok(())
}

/// Stamp the request's branch name (minted at plan approval).
pub fn set_branch(conn: &Connection, id: &str, branch: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("UPDATE requests SET branch = ?2, updated_at = {NOW} WHERE id = ?1"),
        params![id, branch],
    )?;
    Ok(())
}

/// How many requests may execute concurrently per workspace (G4) — each active request gets
/// its own worktree lane; the free lane (the canonical tree) is separate. Requests beyond
/// the cap queue FIFO. Deliberately small: every lane costs a full working copy on disk and
/// a concurrent build's CPU.
pub const MAX_ACTIVE: usize = 2;

/// The workspace's **active** requests — the oldest `limit` that can still *run work*,
/// FIFO: every `executing` one, plus `validating` ones with open tasks (a task restarted
/// mid-review). A validating request merely **parked for its verdict** keeps its tree but
/// takes no dispatch lane: two parked reviews used to hold both slots and starve every
/// queued request in the workspace (the live Empire wedge).
pub fn active_requests(
    conn: &Connection,
    workspace_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<Request>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE workspace_id = ?1 AND (status = 'executing'
             OR (status = 'validating' AND EXISTS (
                   SELECT 1 FROM tasks t WHERE t.request_id = requests.id
                      AND t.status IN ('pending','running','awaiting'))))
          ORDER BY created_at, rowid LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![workspace_id, limit as i64], from_row)?;
    rows.collect()
}

/// Every open (planning / executing) request of a workspace, oldest first — the digest's view.
pub fn open_for_workspace(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<Request>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE workspace_id = ?1 AND status IN ('planning','executing','validating')
          ORDER BY created_at, rowid"
    ))?;
    let rows = stmt.query_map(params![workspace_id], from_row)?;
    rows.collect()
}

/// Record a repo the request touched (first write wins — the branch point never moves).
pub fn record_repo(
    conn: &Connection,
    request_id: &str,
    repo: &str,
    branch: &str,
    base_commit: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO request_repos (request_id, repo, branch, base_commit)
         VALUES (?1, ?2, ?3, ?4)",
        params![request_id, repo, branch, base_commit],
    )?;
    Ok(())
}

/// Every repo the request touched, meta-repo (`""`) first.
pub fn repos_for(conn: &Connection, request_id: &str) -> rusqlite::Result<Vec<RequestRepo>> {
    let mut stmt = conn.prepare(
        "SELECT repo, branch, base_commit, merge_commit FROM request_repos
          WHERE request_id = ?1 ORDER BY repo",
    )?;
    let rows = stmt.query_map(params![request_id], |row| {
        Ok(RequestRepo {
            repo: row.get(0)?,
            branch: row.get(1)?,
            base_commit: row.get(2)?,
            merge_commit: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// Requests that touched a given repo (`projects/<name>` or `''` for the meta-repo), newest
/// first — the project page's changelog in ask-language (P1). Derived from `request_repos`;
/// no project table.
pub fn requests_touching(conn: &Connection, repo: &str) -> rusqlite::Result<Vec<Request>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE id IN (SELECT request_id FROM request_repos WHERE repo = ?1)
          ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map(params![repo], from_row)?;
    rows.collect()
}

/// Stamp a repo's delivery merge commit.
pub fn set_repo_merge(
    conn: &Connection,
    request_id: &str,
    repo: &str,
    merge_commit: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE request_repos SET merge_commit = ?3 WHERE request_id = ?1 AND repo = ?2",
        params![request_id, repo, merge_commit],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{open_in_memory, workspaces};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "W").expect("ws");
        conn
    }

    fn task(conn: &Connection, id: &str, request: Option<&str>) {
        crate::db::endpoints::upsert(conn, "e", "http://e/v1", None, None).expect("e");
        crate::db::tasks::create(
            conn,
            &crate::db::tasks::NewTask {
                id,
                workspace_id: "ws",
                agent: None,
                title: id,
                spec: "do",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("task");
        if let Some(request_id) = request {
            crate::db::tasks::set_request(conn, id, request_id).expect("set request");
        }
    }

    #[test]
    fn lanes_scope_tasks_to_their_request_and_the_free_lane() {
        let conn = seeded();
        create(&conn, "r1", "ws", None, "first", "first").expect("r1");
        create(&conn, "r2", "ws", None, "second", "second").expect("r2");
        set_status(&conn, "r1", RequestStatus::Executing).expect("s");
        set_status(&conn, "r2", RequestStatus::Executing).expect("s");
        task(&conn, "t-r2", Some("r2"));
        task(&conn, "t-r1", Some("r1"));
        task(&conn, "t-free", None);

        // Each lane sees only its own tasks — two active requests dispatch concurrently
        // (G4), and the free lane never picks up request-bound work (or vice versa).
        let pick = |lane: Option<&str>| {
            crate::db::tasks::next_ready(&conn, "ws", lane)
                .expect("scan")
                .map(|task| task.id)
        };
        assert_eq!(pick(Some("r1")).as_deref(), Some("t-r1"));
        assert_eq!(pick(Some("r2")).as_deref(), Some("t-r2"));
        assert_eq!(pick(None).as_deref(), Some("t-free"));

        // A running task holds only its own lane.
        crate::db::tasks::set_status(&conn, "t-r1", crate::db::tasks::TaskStatus::Running)
            .expect("claim");
        assert!(
            crate::db::tasks::running_in_lane(&conn, "ws", Some("r1"))
                .expect("scan")
                .is_some()
        );
        assert!(
            crate::db::tasks::running_in_lane(&conn, "ws", Some("r2"))
                .expect("scan")
                .is_none(),
            "r2's lane stays free while r1's task runs"
        );
        assert!(
            crate::db::tasks::running_in_lane(&conn, "ws", None)
                .expect("scan")
                .is_none()
        );
    }

    #[test]
    fn create_get_and_lifecycle_round_trip() {
        let conn = seeded();
        create(
            &conn,
            "r1",
            "ws",
            Some("chat-1"),
            "Add a thing",
            "add a thing please",
        )
        .expect("create");
        let request = get(&conn, "r1").expect("get").expect("exists");
        assert_eq!(request.status, RequestStatus::Planning);
        assert!(request.branch.is_none());

        set_branch(&conn, "r1", "req/r1-add-a-thing").expect("branch");
        set_status(&conn, "r1", RequestStatus::Executing).expect("status");
        let request = get(&conn, "r1").expect("get").expect("exists");
        assert_eq!(request.branch.as_deref(), Some("req/r1-add-a-thing"));
        assert_eq!(request.status, RequestStatus::Executing);
    }

    fn active_ids(conn: &Connection, limit: usize) -> Vec<String> {
        active_requests(conn, "ws", limit)
            .expect("active")
            .into_iter()
            .map(|req| req.id)
            .collect()
    }

    #[test]
    fn active_requests_are_the_oldest_executing_fifo_capped() {
        let conn = seeded();
        for id in ["r1", "r2", "r3"] {
            create(&conn, id, "ws", None, id, id).expect("create");
        }
        // Nothing executing yet — no active request (planning doesn't hold a lane).
        assert!(
            active_requests(&conn, "ws", MAX_ACTIVE)
                .expect("a")
                .is_empty()
        );

        set_status(&conn, "r1", RequestStatus::Executing).expect("s");
        set_status(&conn, "r2", RequestStatus::Executing).expect("s");
        set_status(&conn, "r3", RequestStatus::Executing).expect("s");
        assert_eq!(
            active_ids(&conn, 2),
            ["r1", "r2"],
            "oldest two hold the lanes; r3 queues"
        );

        // Delivery frees a lane for the next queued request FIFO. A validating request
        // holds a slot only while it still has work to run (a task restarted mid-review)
        // — kept runnable here by r2's pending task.
        task(&conn, "t-r2", Some("r2"));
        set_status(&conn, "r1", RequestStatus::Delivered).expect("s");
        set_status(&conn, "r2", RequestStatus::Validating).expect("s");
        assert_eq!(active_ids(&conn, 2), ["r2", "r3"]);

        // Its restart lands → parked for the verdict → the slot goes to the queue.
        crate::db::tasks::finish(&conn, "t-r2", crate::db::tasks::TaskStatus::Done, None)
            .expect("done");
        assert_eq!(active_ids(&conn, 2), ["r3"]);
    }

    #[test]
    fn parked_validations_do_not_starve_the_lanes() {
        // The live Empire wedge: two requests parked in `validating` (every task done,
        // awaiting a human verdict) held both dispatch slots, so a younger executing
        // request's tasks never ran. Parked validations keep their trees but take no lane.
        let conn = seeded();
        for id in ["r1", "r2", "r3"] {
            create(&conn, id, "ws", None, id, id).expect("create");
        }
        for id in ["r1", "r2"] {
            let task_id = format!("t-{id}");
            task(&conn, &task_id, Some(id));
            crate::db::tasks::finish(&conn, &task_id, crate::db::tasks::TaskStatus::Done, None)
                .expect("done");
            set_status(&conn, id, RequestStatus::Validating).expect("s");
        }
        set_status(&conn, "r3", RequestStatus::Executing).expect("s");
        task(&conn, "t-r3", Some("r3"));

        assert_eq!(
            active_ids(&conn, MAX_ACTIVE),
            ["r3"],
            "the executing request gets a lane past the parked reviews"
        );
        assert_eq!(
            crate::db::tasks::next_ready(&conn, "ws", Some("r3"))
                .expect("scan")
                .expect("some")
                .id,
            "t-r3"
        );
    }

    #[test]
    fn repo_records_keep_the_first_base_and_take_a_merge() {
        let conn = seeded();
        create(&conn, "r1", "ws", None, "t", "a").expect("create");
        record_repo(&conn, "r1", "", "req/r1-t", "aaa").expect("record");
        record_repo(&conn, "r1", "", "req/r1-t", "bbb").expect("ignored");
        record_repo(&conn, "r1", "projects/app", "req/r1-t", "ccc").expect("record");

        set_repo_merge(&conn, "r1", "", "mmm").expect("merge");
        let repos = repos_for(&conn, "r1").expect("repos");
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].base_commit, "aaa", "first base wins");
        assert_eq!(repos[0].merge_commit.as_deref(), Some("mmm"));
        assert_eq!(repos[1].repo, "projects/app");
        assert!(repos[1].merge_commit.is_none());
    }
}
