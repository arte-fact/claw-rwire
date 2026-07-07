//! Central-DB queries for `ssh_keys` — **metadata only** (name + SHA256 fingerprint). The private
//! key never touches the DB; it lives as a `0600` file managed by [`crate::ssh_store`]. This table
//! exists so the admin UI and the per-project remote picker can list keys by a stable identifier.

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshKey {
    pub name: String,
    pub fingerprint: String,
}

/// Insert or update a key's metadata, keeping `created_at` on update (the fingerprint changes when
/// the operator replaces the private file).
pub fn upsert(conn: &Connection, name: &str, fingerprint: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO ssh_keys (name, fingerprint, created_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(name) DO UPDATE SET fingerprint = excluded.fingerprint",
        params![name, fingerprint],
    )?;
    Ok(())
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<SshKey> {
    Ok(SshKey {
        name: row.get(0)?,
        fingerprint: row.get(1)?,
    })
}

const SELECT: &str = "SELECT name, fingerprint FROM ssh_keys";

pub fn get(conn: &Connection, name: &str) -> rusqlite::Result<Option<SshKey>> {
    conn.query_row(
        &format!("{SELECT} WHERE name = ?1"),
        params![name],
        from_row,
    )
    .optional()
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<SshKey>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY name ASC"))?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

pub fn delete(conn: &Connection, name: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM ssh_keys WHERE name = ?1", params![name])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn upsert_get_list_delete_roundtrip() {
        let conn = open_in_memory().expect("db");
        upsert(&conn, "b-key", "SHA256:bbb").expect("b");
        upsert(&conn, "a-key", "SHA256:aaa").expect("a");
        let names: Vec<String> = list(&conn)
            .expect("list")
            .into_iter()
            .map(|k| k.name)
            .collect();
        assert_eq!(names, vec!["a-key".to_owned(), "b-key".to_owned()]);

        assert_eq!(
            get(&conn, "a-key").expect("get").expect("some").fingerprint,
            "SHA256:aaa"
        );
        // Re-upsert replaces the fingerprint in place (a rotated key).
        upsert(&conn, "a-key", "SHA256:zzz").expect("rotate");
        assert_eq!(list(&conn).expect("list").len(), 2, "no duplicate row");
        assert_eq!(
            get(&conn, "a-key").expect("get").expect("some").fingerprint,
            "SHA256:zzz"
        );

        delete(&conn, "a-key").expect("delete");
        assert!(get(&conn, "a-key").expect("get").is_none());
    }
}
