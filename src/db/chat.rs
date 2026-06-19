//! Central-DB queries for the web chat surface: threads are `messaging_groups`
//! rows with `channel_type='web'`; turns are `web_messages` rows (`direction`
//! `in` = operator, `out` = agent). Timestamps are stamped by SQLite.

use rusqlite::Connection;

use crate::state::{ChatMessage, ChatSummary, MessageRole};

const WEB: &str = "web";

fn direction(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "in",
        MessageRole::Agent => "out",
    }
}

fn sender(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "you",
        MessageRole::Agent => "claw",
    }
}

fn role_from_direction(direction: &str) -> MessageRole {
    match direction {
        "out" => MessageRole::Agent,
        _ => MessageRole::User,
    }
}

pub fn list_chats(conn: &Connection) -> rusqlite::Result<Vec<ChatSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(NULLIF(name, ''), 'New chat')
           FROM messaging_groups
          WHERE channel_type = ?1 AND archived_at IS NULL
          ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([WEB], |row| {
        Ok(ChatSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            busy: false,
        })
    })?;
    rows.collect()
}

pub fn create_chat(conn: &Connection, id: &str, title: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO messaging_groups (id, channel_type, platform_id, name, created_at)
         VALUES (?1, ?2, ?1, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        rusqlite::params![id, WEB, title],
    )?;
    Ok(())
}

/// Title a thread from its first message, but only while it still carries the
/// default name — so the operator's later renames (if any) stick.
pub fn title_if_default(conn: &Connection, id: &str, title: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE messaging_groups SET name = ?2
          WHERE id = ?1 AND (name IS NULL OR name = '' OR name = 'New chat')",
        rusqlite::params![id, title],
    )?;
    Ok(())
}

pub fn load_thread(conn: &Connection, chat_id: &str) -> rusqlite::Result<Vec<ChatMessage>> {
    let mut stmt = conn.prepare(
        "SELECT direction, body, substr(created_at, 12, 5)
           FROM web_messages
          WHERE messaging_group_id = ?1 AND kind = 'chat'
          ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([chat_id], |row| {
        let direction: String = row.get(0)?;
        Ok(ChatMessage {
            role: role_from_direction(&direction),
            text: row.get(1)?,
            time: row.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn append_message(
    conn: &Connection,
    chat_id: &str,
    role: MessageRole,
    body: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO web_messages (messaging_group_id, direction, sender, body, created_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        rusqlite::params![chat_id, direction(role), sender(role), body],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn create_list_append_and_load_roundtrip() {
        let conn = db::open_in_memory().expect("open");
        create_chat(&conn, "c1", "First").expect("create");
        create_chat(&conn, "c2", "Second").expect("create");

        let chats = list_chats(&conn).expect("list");
        assert_eq!(chats.len(), 2);
        // Newest first.
        assert_eq!(chats[0].id, "c2");

        append_message(&conn, "c1", MessageRole::User, "hello").expect("append");
        append_message(&conn, "c1", MessageRole::Agent, "hi back").expect("append");

        let thread = load_thread(&conn, "c1").expect("load");
        let shape: Vec<(MessageRole, &str)> =
            thread.iter().map(|m| (m.role, m.text.as_str())).collect();
        assert_eq!(
            shape,
            vec![
                (MessageRole::User, "hello"),
                (MessageRole::Agent, "hi back"),
            ]
        );
        assert!(load_thread(&conn, "c2").expect("load empty").is_empty());
    }

    #[test]
    fn title_if_default_only_renames_default_threads() {
        let conn = db::open_in_memory().expect("open");
        create_chat(&conn, "c1", "New chat").expect("create");
        create_chat(&conn, "c2", "Kept name").expect("create");
        title_if_default(&conn, "c1", "From message").expect("rename");
        title_if_default(&conn, "c2", "Should not apply").expect("rename");
        let by_id: std::collections::HashMap<String, String> = list_chats(&conn)
            .expect("list")
            .into_iter()
            .map(|c| (c.id, c.title))
            .collect();
        assert_eq!(by_id["c1"], "From message");
        assert_eq!(by_id["c2"], "Kept name");
    }
}
