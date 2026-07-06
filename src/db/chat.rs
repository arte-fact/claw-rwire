//! Central-DB queries for the web chat surface: threads are `messaging_groups`
//! rows with `channel_type='web'`; turns are `web_messages` rows (`direction`
//! `in` = operator, `out` = agent). Timestamps are stamped by SQLite.

use rusqlite::{Connection, OptionalExtension};

use crate::state::{ChatMessage, ChatSummary, MessageRole, ToolCardView, TranscriptItem};

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
        })
    })?;
    rows.collect()
}

/// Archived threads (`archived_at` set), most-recently-archived first. The sidebar's
/// "Archived" section reads this; the active `list_chats` excludes these rows.
pub fn list_archived_chats(conn: &Connection) -> rusqlite::Result<Vec<ChatSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(NULLIF(name, ''), 'New chat')
           FROM messaging_groups
          WHERE channel_type = ?1 AND archived_at IS NOT NULL
          ORDER BY archived_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([WEB], |row| {
        Ok(ChatSummary {
            id: row.get(0)?,
            title: row.get(1)?,
        })
    })?;
    rows.collect()
}

/// Collapse to **one** persistent concierge conversation (roadmap R1): keep the single most
/// recently *active* web chat — the one with the newest message, falling back to the newest
/// created among empty ones — and archive every other active chat. Returns the survivor's id.
/// Idempotent (one active chat → no-op). This is also what makes the concierge's rolling-memory
/// marker coherent: one thread ↔ one `summarized_through`.
pub fn collapse_to_primary(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let primary: Option<String> = conn
        .query_row(
            "SELECT g.id FROM messaging_groups g
              WHERE g.channel_type = ?1 AND g.archived_at IS NULL
              ORDER BY (SELECT COALESCE(MAX(m.id), 0) FROM web_messages m
                         WHERE m.messaging_group_id = g.id) DESC,
                       g.created_at DESC, g.id DESC
              LIMIT 1",
            [WEB],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = &primary {
        conn.execute(
            "UPDATE messaging_groups
                SET archived_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE channel_type = ?1 AND archived_at IS NULL AND id != ?2",
            rusqlite::params![WEB, id],
        )?;
    }
    Ok(primary)
}

/// Permanently delete a thread and everything that hangs off it: its transcript
/// (`web_messages`) and any agent wiring (`messaging_group_agents`), then the group
/// row itself. Children are deleted first so no orphans survive even with foreign
/// keys off (the default). Irreversible — the UI gates this behind a confirm.
pub fn delete_chat(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM web_messages WHERE messaging_group_id = ?1",
        [id],
    )?;
    conn.execute(
        "DELETE FROM messaging_group_agents WHERE messaging_group_id = ?1",
        [id],
    )?;
    conn.execute(
        "DELETE FROM messaging_groups WHERE id = ?1 AND channel_type = ?2",
        rusqlite::params![id, WEB],
    )?;
    Ok(())
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

/// The rendered thread: chat turns and tool cards interleaved by insertion order — the newest
/// `window` turns (oldest-first), plus the count of older rows *not* loaded (the seamless-history
/// sentinel's `next`, P4a). `kind='tool'` rows join their structured payload from
/// `web_message_tools`. The window grows a page at a time as the reader scrolls up (the bridge's
/// `OPEN_CHAT` store owns the size).
pub fn load_thread_windowed(
    conn: &Connection,
    chat_id: &str,
    window: usize,
) -> rusqlite::Result<(Vec<TranscriptItem>, usize)> {
    let total: usize = conn.query_row(
        "SELECT COUNT(*) FROM web_messages
          WHERE messaging_group_id = ?1 AND kind IN ('chat', 'tool')",
        [chat_id],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT w.kind, w.direction, w.body, substr(w.created_at, 12, 5),
                t.name, t.arguments, t.result, t.status, w.id
           FROM (SELECT id, kind, direction, body, created_at
                   FROM web_messages
                  WHERE messaging_group_id = ?1 AND kind IN ('chat', 'tool')
                  ORDER BY id DESC LIMIT ?2) w
           LEFT JOIN web_message_tools t ON t.web_message_id = w.id
          ORDER BY w.id ASC",
    )?;
    let items: Vec<TranscriptItem> = stmt
        .query_map((chat_id, window as i64), transcript_row)?
        .collect::<rusqlite::Result<_>>()?;
    let truncated = total.saturating_sub(items.len());
    Ok((items, truncated))
}

/// Map one joined `web_messages`(+tool) row to a transcript item. Column 8 is the `web_messages`
/// rowid — the item's stable key for keyed morph reuse ([`TranscriptItem`]).
fn transcript_row(row: &rusqlite::Row) -> rusqlite::Result<TranscriptItem> {
    let kind: String = row.get(0)?;
    let time: String = row.get(3)?;
    let id: i64 = row.get(8)?;
    if kind == "tool" {
        Ok(TranscriptItem::Tool {
            id,
            card: ToolCardView {
                name: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                arguments: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                result: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                ok: row.get::<_, Option<String>>(7)?.as_deref() == Some("ok"),
                time,
            },
        })
    } else {
        let direction: String = row.get(1)?;
        Ok(TranscriptItem::Message {
            id,
            message: ChatMessage {
                role: role_from_direction(&direction),
                text: row.get(2)?,
                time,
            },
        })
    }
}

/// One model-facing transcript turn with its source `web_messages.id` — the id lets the
/// rolling-memory summarizer mark how far it has consumed the thread.
pub struct ModelTurn {
    pub id: i64,
    pub role: MessageRole,
    pub text: String,
}

/// The transcript flattened for the model, only the turns *after* `after_id` (pass `0` for
/// all): chat turns plus each tool turn folded in as a short assistant note (`body`). The
/// rolling memory passes its `summarized_through` marker so already-condensed turns are
/// dropped and only the recent tail is sent raw.
pub fn model_turns_after(
    conn: &Connection,
    chat_id: &str,
    after_id: i64,
) -> rusqlite::Result<Vec<ModelTurn>> {
    let mut stmt = conn.prepare(
        "SELECT id, direction, body, kind
           FROM web_messages
          WHERE messaging_group_id = ?1 AND kind IN ('chat', 'tool') AND id > ?2
          ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![chat_id, after_id], |row| {
        let id: i64 = row.get(0)?;
        let direction: String = row.get(1)?;
        let body: String = row.get(2)?;
        let kind: String = row.get(3)?;
        let role = if kind == "tool" {
            MessageRole::Agent
        } else {
            role_from_direction(&direction)
        };
        Ok(ModelTurn {
            id,
            role,
            text: body,
        })
    })?;
    rows.collect()
}

/// Active chats whose most recent message is inbound (`direction='in'`) — a user turn the
/// concierge never answered. On a clean run this is empty (a reply or a tool row always
/// follows); a non-empty result means a worker thread died mid-turn (a restart), and boot
/// recovery re-dispatches these. Ordered oldest-first so recovery replays in arrival order.
pub fn chats_awaiting_reply(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT g.id FROM messaging_groups g
           JOIN web_messages m ON m.id = (
               SELECT MAX(m2.id) FROM web_messages m2 WHERE m2.messaging_group_id = g.id
           )
          WHERE g.channel_type = ?1 AND g.archived_at IS NULL AND m.direction = 'in'
          ORDER BY m.id ASC",
    )?;
    let rows = stmt.query_map([WEB], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// The body of the chat's most recent **inbound** message — the user turn boot recovery
/// replays (echo path) or resumes (the native path re-reads it from the transcript). `None`
/// when the latest message isn't inbound (a reply already landed — nothing to recover).
pub fn last_user_text(conn: &Connection, chat_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT body, direction FROM web_messages
          WHERE messaging_group_id = ?1
          ORDER BY id DESC LIMIT 1",
        rusqlite::params![chat_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .map(|(body, direction)| (direction == "in").then_some(body))
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
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

/// Persist one tool invocation: a `kind='tool'` row carrying a compact model-facing
/// note in `body` (capped, for cross-turn replay), plus the structured sidecar the UI
/// card reads.
pub fn append_tool(
    conn: &Connection,
    chat_id: &str,
    name: &str,
    arguments: &str,
    result: &str,
    ok: bool,
) -> rusqlite::Result<()> {
    // Phrase the replay as a past-tense *observation*, not a call. The model sees prior
    // tool turns in its context; the earlier `[tool:{name}] …` framing read like an
    // invocation, and the model learned to imitate it — emitting `[tool:ask] …` as prose
    // instead of a real tool call, so those delegations silently no-op'd. "result of" reads
    // as something that already happened.
    let note = format!("[result of {name}]\n{}", truncate(result, 2000));
    conn.execute(
        "INSERT INTO web_messages (messaging_group_id, direction, sender, body, kind, created_at)
         VALUES (?1, 'out', 'claw', ?2, 'tool', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        rusqlite::params![chat_id, note],
    )?;
    let web_message_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO web_message_tools (web_message_id, name, arguments, result, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        rusqlite::params![
            web_message_id,
            name,
            arguments,
            result,
            if ok { "ok" } else { "error" }
        ],
    )?;
    Ok(())
}

/// Char-aware truncation with an ellipsis — keeps the model-facing tool note bounded.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
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

        let (thread, truncated) = load_thread_windowed(&conn, "c1", 100).expect("load");
        assert_eq!(truncated, 0, "the whole short thread fits the window");
        let shape: Vec<(MessageRole, &str)> = thread
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Message { message: m, .. } => Some((m.role, m.text.as_str())),
                TranscriptItem::Tool { .. } => None,
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (MessageRole::User, "hello"),
                (MessageRole::Agent, "hi back"),
            ]
        );
        assert!(
            load_thread_windowed(&conn, "c2", 100)
                .expect("load empty")
                .0
                .is_empty()
        );
    }

    #[test]
    fn windowing_keeps_the_newest_turns_and_counts_the_rest() {
        let conn = db::open_in_memory().expect("open");
        create_chat(&conn, "c1", "Long").expect("create");
        for n in 0..5 {
            append_message(&conn, "c1", MessageRole::User, &format!("m{n}")).expect("append");
        }
        // A window of 2 loads only the newest two turns, oldest-first, and reports 3 older.
        let (thread, truncated) = load_thread_windowed(&conn, "c1", 2).expect("load");
        assert_eq!(truncated, 3);
        let texts: Vec<&str> = thread
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Message { message: m, .. } => Some(m.text.as_str()),
                TranscriptItem::Tool { .. } => None,
            })
            .collect();
        assert_eq!(texts, vec!["m3", "m4"], "newest two, oldest-first");
    }

    #[test]
    fn tool_turns_interleave_in_the_thread_and_fold_into_the_model_view() {
        let conn = db::open_in_memory().expect("open");
        create_chat(&conn, "c1", "T").expect("create");
        append_message(&conn, "c1", MessageRole::User, "search rust").expect("user");
        append_tool(
            &conn,
            "c1",
            "search",
            r#"{"query":"rust"}"#,
            "Rust is a language.",
            true,
        )
        .expect("tool");
        append_message(&conn, "c1", MessageRole::Agent, "Rust is great.").expect("reply");

        let (thread, _) = load_thread_windowed(&conn, "c1", 100).expect("load");
        assert_eq!(thread.len(), 3);
        assert!(
            matches!(&thread[0], TranscriptItem::Message { message: m, .. } if m.role == MessageRole::User)
        );
        match &thread[1] {
            TranscriptItem::Tool { card, .. } => {
                assert_eq!(card.name, "search");
                assert!(card.ok);
                assert!(card.result.contains("Rust is a language"));
            }
            TranscriptItem::Message { .. } => panic!("expected a tool card"),
        }
        assert!(
            matches!(&thread[2], TranscriptItem::Message { message: m, .. } if m.role == MessageRole::Agent)
        );

        // The model view folds the tool into a note between the chat turns.
        let turns = model_turns_after(&conn, "c1", 0).expect("model");
        assert_eq!(turns.len(), 3);
        assert!(turns[1].text.starts_with("[result of search]"));
        // The marker windows the tail: only turns after the first id remain.
        let after_first = model_turns_after(&conn, "c1", turns[0].id).expect("after");
        assert_eq!(after_first.len(), 2);
    }

    #[test]
    fn collapse_to_primary_keeps_the_most_recently_active_chat() {
        let conn = db::open_in_memory().expect("open");
        // c1 is older but holds the live conversation; c2 is newer but empty.
        create_chat(&conn, "c1", "The conversation").expect("c1");
        create_chat(&conn, "c2", "New chat").expect("c2");
        append_message(&conn, "c1", MessageRole::User, "hello").expect("append");

        let primary = collapse_to_primary(&conn).expect("collapse");
        assert_eq!(primary.as_deref(), Some("c1"), "activity beats recency");
        let active: Vec<String> = list_chats(&conn)
            .expect("list")
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(active, vec!["c1".to_owned()]);
        assert_eq!(
            list_archived_chats(&conn).expect("archived")[0].id,
            "c2",
            "the losers stay reachable under Archived"
        );

        // Idempotent: a second collapse changes nothing.
        assert_eq!(
            collapse_to_primary(&conn).expect("again").as_deref(),
            Some("c1")
        );
        assert_eq!(list_chats(&conn).expect("list").len(), 1);
        assert_eq!(list_archived_chats(&conn).expect("archived").len(), 1);
    }

    #[test]
    fn collapse_to_primary_on_no_chats_is_none() {
        let conn = db::open_in_memory().expect("open");
        assert_eq!(collapse_to_primary(&conn).expect("collapse"), None);
    }

    #[test]
    fn delete_removes_the_thread_and_its_messages() {
        let conn = db::open_in_memory().expect("open");
        create_chat(&conn, "c1", "Keep").expect("create");
        create_chat(&conn, "c2", "Doomed").expect("create");
        append_message(&conn, "c2", MessageRole::User, "trace").expect("append");

        delete_chat(&conn, "c2").expect("delete");

        let active: Vec<String> = list_chats(&conn)
            .expect("list")
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(active, vec!["c1".to_owned()]);
        assert!(list_archived_chats(&conn).expect("archived").is_empty());
        // The transcript is gone, not orphaned.
        let leftover: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM web_messages WHERE messaging_group_id = ?1",
                ["c2"],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(leftover, 0);
    }

    #[test]
    fn awaiting_reply_finds_only_unanswered_inbound_turns() {
        let conn = db::open_in_memory().expect("open");
        create_chat(&conn, "answered", "a").expect("create");
        create_chat(&conn, "pending", "b").expect("create");
        create_chat(&conn, "empty", "c").expect("create");
        // answered: user then agent — a reply landed, nothing to recover.
        append_message(&conn, "answered", MessageRole::User, "hi").expect("u");
        append_message(&conn, "answered", MessageRole::Agent, "hello").expect("a");
        // pending: user with no reply after — a turn died mid-flight.
        append_message(&conn, "pending", MessageRole::User, "do the thing").expect("u");

        let awaiting = chats_awaiting_reply(&conn).expect("query");
        assert_eq!(awaiting, vec!["pending".to_owned()]);
        assert_eq!(
            last_user_text(&conn, "pending").expect("text").as_deref(),
            Some("do the thing")
        );
        assert_eq!(last_user_text(&conn, "answered").expect("text"), None);
        assert_eq!(last_user_text(&conn, "empty").expect("text"), None);
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
