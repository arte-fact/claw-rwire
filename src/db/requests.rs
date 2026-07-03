//! Requests (G1 of `docs/requests.md`): the first-class unit above tasks — one user ask,
//! from delegation to delivery. A Request owns its plan and tasks (`tasks.request_id`), maps
//! onto one branch name across every repo it touches (`request_repos` carries the per-repo
//! branch point and merge commit), and the scheduler lane only picks tasks of the **active**
//! (oldest executing) request — queued requests activate FIFO as earlier ones deliver.

use rusqlite::{Connection, OptionalExtension, params};

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// The request lifecycle. `Planning` covers ask → plan-review (the plan task's own status
/// distinguishes the gate); `Executing` starts at plan approval — the moment a branch can be
/// cut; the terminal states end the lane's claim on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    Planning,
    Executing,
    Delivered,
    Rejected,
    Abandoned,
}

impl RequestStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Executing => "executing",
            Self::Delivered => "delivered",
            Self::Rejected => "rejected",
            Self::Abandoned => "abandoned",
        }
    }

    #[must_use]
    pub fn from_str(value: &str) -> Self {
        match value {
            "executing" => Self::Executing,
            "delivered" => Self::Delivered,
            "rejected" => Self::Rejected,
            "abandoned" => Self::Abandoned,
            _ => Self::Planning,
        }
    }
}

/// `created_at` stays in the table but off the struct until something reads it.
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

const SELECT: &str = "SELECT id, workspace_id, origin_chat_id, title, ask, status, branch \
                      FROM requests";

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Request> {
    Ok(Request {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        origin_chat: row.get(2)?,
        title: row.get(3)?,
        ask: row.get(4)?,
        status: RequestStatus::from_str(&row.get::<_, String>(5)?),
        branch: row.get(6)?,
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

/// Stamp the request's branch name (minted at plan approval).
pub fn set_branch(conn: &Connection, id: &str, branch: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("UPDATE requests SET branch = ?2, updated_at = {NOW} WHERE id = ?1"),
        params![id, branch],
    )?;
    Ok(())
}

/// The workspace's **active** request — the oldest `executing` one. The scheduler lane only
/// picks tasks of this request (plus free-standing tasks); the rest queue FIFO behind it.
pub fn active_for_workspace(
    conn: &Connection,
    workspace_id: &str,
) -> rusqlite::Result<Option<Request>> {
    conn.query_row(
        &format!(
            "{SELECT} WHERE workspace_id = ?1 AND status = 'executing'
              ORDER BY created_at LIMIT 1"
        ),
        params![workspace_id],
        from_row,
    )
    .optional()
}

/// Every open (planning / executing) request of a workspace, oldest first — the digest's view.
pub fn open_for_workspace(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<Request>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE workspace_id = ?1 AND status IN ('planning','executing')
          ORDER BY created_at"
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
    fn lane_scoping_only_runs_the_active_requests_tasks() {
        let conn = seeded();
        create(&conn, "r1", "ws", None, "first", "first").expect("r1");
        create(&conn, "r2", "ws", None, "second", "second").expect("r2");
        set_status(&conn, "r1", RequestStatus::Executing).expect("s");
        set_status(&conn, "r2", RequestStatus::Executing).expect("s");
        task(&conn, "t-r2", Some("r2")); // oldest task, but its request is queued
        task(&conn, "t-r1", Some("r1"));
        task(&conn, "t-free", None);

        // Only the active request's task and the free-standing one are eligible.
        let first = crate::db::tasks::next_ready(&conn, "ws")
            .expect("scan")
            .expect("some");
        assert_ne!(first.id, "t-r2", "a queued request's task must wait");
        crate::db::tasks::finish(&conn, "t-r1", crate::db::tasks::TaskStatus::Done, None)
            .expect("done");
        crate::db::tasks::finish(&conn, "t-free", crate::db::tasks::TaskStatus::Done, None)
            .expect("done");
        assert!(
            crate::db::tasks::next_ready(&conn, "ws")
                .expect("scan")
                .is_none(),
            "r2's task stays invisible while r1 is still the active request"
        );

        // Delivering r1 activates r2 FIFO — its task becomes eligible.
        set_status(&conn, "r1", RequestStatus::Delivered).expect("s");
        assert_eq!(
            crate::db::tasks::next_ready(&conn, "ws")
                .expect("scan")
                .expect("some")
                .id,
            "t-r2"
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

    #[test]
    fn active_request_is_the_oldest_executing_fifo() {
        let conn = seeded();
        for id in ["r1", "r2", "r3"] {
            create(&conn, id, "ws", None, id, id).expect("create");
        }
        // Nothing executing yet — no active request (planning doesn't hold the lane).
        assert!(active_for_workspace(&conn, "ws").expect("active").is_none());

        set_status(&conn, "r2", RequestStatus::Executing).expect("s");
        set_status(&conn, "r3", RequestStatus::Executing).expect("s");
        assert_eq!(
            active_for_workspace(&conn, "ws")
                .expect("active")
                .expect("some")
                .id,
            "r2",
            "oldest executing wins"
        );
        // Delivery frees the lane for the next queued request.
        set_status(&conn, "r2", RequestStatus::Delivered).expect("s");
        assert_eq!(
            active_for_workspace(&conn, "ws")
                .expect("active")
                .expect("some")
                .id,
            "r3"
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
