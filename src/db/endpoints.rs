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

pub fn is_empty(conn: &Connection) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM endpoints", [], |row| row.get(0))?;
    Ok(count == 0)
}

pub fn insert(
    conn: &Connection,
    name: &str,
    base_url: &str,
    api_key: Option<&str>,
    api_key_env: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO endpoints (name, base_url, api_key, api_key_env, created_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![name, base_url, api_key, api_key_env],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, name: &str) -> rusqlite::Result<Option<Endpoint>> {
    conn.query_row(
        "SELECT name, base_url, api_key, api_key_env FROM endpoints WHERE name = ?1",
        params![name],
        |row| {
            Ok(Endpoint {
                name: row.get(0)?,
                base_url: row.get(1)?,
                api_key: row.get(2)?,
                api_key_env: row.get(3)?,
            })
        },
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn insert_then_get_roundtrips_and_empty_tracks_rows() {
        let conn = open_in_memory().expect("db");
        assert!(is_empty(&conn).expect("empty"));
        insert(&conn, "default", "http://x/v1", Some("sk-1"), None).expect("insert");
        assert!(!is_empty(&conn).expect("not empty"));
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
}
