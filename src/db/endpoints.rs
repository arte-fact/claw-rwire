//! Central-DB queries for `endpoints` — the OpenAI-compatible API targets. A row's
//! key is either stored inline (`api_key`) or named via an env var (`api_key_env`),
//! resolved in [`super::resolution`].

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
}

/// Insert or update an endpoint, keeping `created_at`. Used to keep the env-managed
/// `default` endpoint in sync with `.env` on each boot.
pub fn upsert(
    conn: &Connection,
    name: &str,
    base_url: &str,
    api_key: Option<&str>,
    api_key_env: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO endpoints (name, base_url, api_key, api_key_env, created_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(name) DO UPDATE SET
           base_url = excluded.base_url,
           api_key = excluded.api_key,
           api_key_env = excluded.api_key_env",
        params![name, base_url, api_key, api_key_env],
    )?;
    Ok(())
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Endpoint> {
    Ok(Endpoint {
        name: row.get(0)?,
        base_url: row.get(1)?,
        api_key: row.get(2)?,
        api_key_env: row.get(3)?,
    })
}

const SELECT: &str = "SELECT name, base_url, api_key, api_key_env FROM endpoints";

pub fn get(conn: &Connection, name: &str) -> rusqlite::Result<Option<Endpoint>> {
    conn.query_row(
        &format!("{SELECT} WHERE name = ?1"),
        params![name],
        from_row,
    )
    .optional()
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Endpoint>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY name ASC"))?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

pub fn delete(conn: &Connection, name: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM endpoints WHERE name = ?1", params![name])?;
    Ok(())
}

pub fn is_empty(conn: &Connection) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM endpoints", [], |row| row.get(0))?;
    Ok(count == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM endpoints", [], |row| row.get(0))
            .expect("count")
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let conn = open_in_memory().expect("db");
        upsert(&conn, "default", "http://x/v1", Some("sk-1"), None).expect("upsert");
        let got = get(&conn, "default").expect("get").expect("some");
        assert_eq!(
            got,
            Endpoint {
                name: "default".to_owned(),
                base_url: "http://x/v1".to_owned(),
                api_key: Some("sk-1".to_owned()),
                api_key_env: None,
            }
        );
        assert!(get(&conn, "missing").expect("get").is_none());
    }

    #[test]
    fn list_is_sorted_and_delete_removes() {
        let conn = open_in_memory().expect("db");
        assert!(is_empty(&conn).expect("empty"));
        upsert(&conn, "b", "http://b/v1", None, None).expect("b");
        upsert(&conn, "a", "http://a/v1", None, None).expect("a");
        let names: Vec<String> = list(&conn)
            .expect("list")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["a".to_owned(), "b".to_owned()]);
        delete(&conn, "a").expect("delete");
        assert_eq!(count(&conn), 1);
        assert!(get(&conn, "a").expect("get").is_none());
    }

    #[test]
    fn upsert_updates_base_url_in_place() {
        let conn = open_in_memory().expect("db");
        upsert(&conn, "default", "https://old/v1", Some("k1"), None).expect("insert");
        upsert(&conn, "default", "http://new/v1", None, None).expect("update");
        let got = get(&conn, "default").expect("get").expect("some");
        assert_eq!(got.base_url, "http://new/v1");
        assert_eq!(got.api_key, None);
        assert_eq!(count(&conn), 1, "upsert must not add a second row");
    }
}
