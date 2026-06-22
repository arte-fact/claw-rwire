//! Global key/value config (the `settings` table): editable defaults that override the
//! compiled-in constants — the role preambles and memory knobs (Phase E). A blank/absent
//! value means "use the compiled default", so clearing a setting resets it.

use rusqlite::{Connection, OptionalExtension, params};

/// The override for `key`, or `None` if unset/blank (caller then uses its compiled default).
pub fn get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()))
}

/// Set (or, with a blank value, clear) the override for `key`.
pub fn set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    if value.trim().is_empty() {
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// The override for `key`, or `default` when unset.
pub fn get_or(conn: &Connection, key: &str, default: &str) -> String {
    get(conn, key)
        .ok()
        .flatten()
        .unwrap_or_else(|| default.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn set_get_and_blank_clears() {
        let conn = open_in_memory().expect("db");
        assert_eq!(get(&conn, "role.concierge").expect("get"), None);
        set(&conn, "role.concierge", "Be terse.").expect("set");
        assert_eq!(
            get(&conn, "role.concierge").expect("get").as_deref(),
            Some("Be terse.")
        );
        assert_eq!(get_or(&conn, "role.concierge", "DEFAULT"), "Be terse.");
        assert_eq!(get_or(&conn, "missing", "DEFAULT"), "DEFAULT");
        // Blank clears → falls back to default.
        set(&conn, "role.concierge", "  ").expect("clear");
        assert_eq!(get(&conn, "role.concierge").expect("get"), None);
    }
}
