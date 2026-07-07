//! Per-project git-remote settings (P5b): the deploy origin URL and the chosen SSH deploy-key name,
//! stored in the shared `settings` table under `project.<ws>.<name>.{origin,deploy_key}` — the same
//! namespace as the kind override (`kinds::kind_key`) and the preview override. The key *material*
//! lives in [`crate::ssh_store`]; here we keep only the key's name so a project can point at it.

use rusqlite::Connection;

use crate::db::settings;

fn origin_key(workspace: &str, project: &str) -> String {
    format!("project.{workspace}.{project}.origin")
}

fn deploy_key_key(workspace: &str, project: &str) -> String {
    format!("project.{workspace}.{project}.deploy_key")
}

/// The configured push URL for a project, or `None` when unset.
pub fn origin(conn: &Connection, workspace: &str, project: &str) -> Option<String> {
    settings::get(conn, &origin_key(workspace, project))
        .ok()
        .flatten()
}

/// The configured deploy-key name for a project, or `None` when unset.
pub fn deploy_key(conn: &Connection, workspace: &str, project: &str) -> Option<String> {
    settings::get(conn, &deploy_key_key(workspace, project))
        .ok()
        .flatten()
}

/// Set (or clear, with a blank value) a project's origin URL + deploy-key name together.
pub fn set(
    conn: &Connection,
    workspace: &str,
    project: &str,
    origin: &str,
    deploy_key: &str,
) -> rusqlite::Result<()> {
    settings::set(conn, &origin_key(workspace, project), origin)?;
    settings::set(conn, &deploy_key_key(workspace, project), deploy_key)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn set_reads_back_and_blank_clears() {
        let conn = open_in_memory().expect("db");
        assert_eq!(origin(&conn, "ws", "site"), None);
        assert_eq!(deploy_key(&conn, "ws", "site"), None);

        set(
            &conn,
            "ws",
            "site",
            "git@github.com:me/site.git",
            "github-deploy",
        )
        .expect("set");
        assert_eq!(
            origin(&conn, "ws", "site").as_deref(),
            Some("git@github.com:me/site.git")
        );
        assert_eq!(
            deploy_key(&conn, "ws", "site").as_deref(),
            Some("github-deploy")
        );
        // Scoped per (workspace, project) — a sibling project is unaffected.
        assert_eq!(origin(&conn, "ws", "other"), None);

        set(&conn, "ws", "site", "", "").expect("clear");
        assert_eq!(origin(&conn, "ws", "site"), None);
        assert_eq!(deploy_key(&conn, "ws", "site"), None);
    }
}
