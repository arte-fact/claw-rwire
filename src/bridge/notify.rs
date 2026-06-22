//! The concierge's completion notice. When a delegated task finishes, the concierge (the
//! chat agent the delegation came from) runs a short turn to phrase the outcome in its own
//! voice, posted into the origin chat. One model call per completion — accepted, for a
//! notice in the concierge's own voice rather than a templated string.

use crate::agent;
use crate::db::{self, resolution};
use crate::state::{ChatMessage, MessageRole};
use crate::tools::EmptyToolbox;

use super::{Command, data_dir, log_line, now_hm, send};

/// Spawn the concierge notify turn for a finished task. Best-effort: a resolve/turn failure
/// just logs — a background notice must not raise an error banner or block the scheduler.
pub fn completion(origin_chat: &str, title: &str, status: &str, result: &str) {
    let origin_chat = origin_chat.to_owned();
    let summary = format!("Task: {title}\nStatus: {status}\nResult:\n{result}");
    std::thread::spawn(move || {
        let conn = match db::open(&data_dir().join("central.db")) {
            Ok(conn) => conn,
            Err(err) => return log_line(format!("notify: open db failed: {err}")),
        };
        let inference = match resolution::resolve_for_chat(&conn, &origin_chat, |key| {
            std::env::var(key).ok()
        }) {
            Ok(inference) => inference,
            Err(err) => return log_line(format!("notify: resolve concierge failed: {err}")),
        };
        let system = format!(
            "{}\n\nA task you delegated to a workspace has just finished. In one or two \
             sentences, tell the user the outcome in your own voice.",
            agent::SYSTEM_PROMPT
        );
        let user = ChatMessage {
            role: MessageRole::User,
            text: summary,
            time: now_hm(),
        };
        let messages = agent::build_messages(&system, std::slice::from_ref(&user));
        match agent::run_turn(&inference, messages, &EmptyToolbox) {
            Ok(turn) => {
                let text = turn.reply.unwrap_or_default();
                if !text.trim().is_empty() {
                    send(Command::Notified {
                        chat: origin_chat,
                        text,
                    });
                }
            }
            Err(err) => log_line(format!("notify: turn failed: {err}")),
        }
    });
}
