//! Review projection: tasks parked at a human gate are reviewed *in the workspace* (the board's
//! "Needs you" filter + the Peek modal's Review tab) and, since Phase 2, per **agent** in the
//! sidebar's agent list. There's no cross-workspace inbox; the "needs you" counts live on the
//! agent-list nav (`agent_nav`), which carries both the per-agent badge and the per-workspace
//! group total. A verdict from the board/Peek drives the loop via `scheduler::human_verdict`.

use rwire::SharedServerState;

/// Refresh the "needs you" projection after a task parks or resolves — now the agent-list nav,
/// which stamps each agent's status + approval badge and each workspace group's total. Cheap
/// (a few grouped queries); called on each tick and whenever a gate opens or closes.
pub fn project_awaiting(conn: &rusqlite::Connection, shared: &SharedServerState) {
    super::agent_nav::project(conn, shared);
}
