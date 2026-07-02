//! Startup: the one-time pass the bridge runs before its first command — seed the provider
//! and personas from the environment, collapse legacy chats, sweep orphaned running tasks,
//! and log the sandbox posture. Split from `bridge/mod.rs`; runs on the bridge thread.

use rwire::{ChangeSet, SharedServerState};

use crate::db::{self, agents, chat, endpoints, workspaces};
use crate::id;
use crate::state::App;

use super::{admin, data_dir, log_line, project_chat_memory, project_memory_overview, seed};

/// Prime the projection from the DB and select the most recent thread. On first
/// run (no threads yet) seed one so the operator lands in a ready chat.
pub(super) fn boot(conn: &rusqlite::Connection, shared: &SharedServerState) {
    if chat::list_chats(conn).map(|chats| chats.is_empty()) == Ok(true)
        && let Ok(id) = id::new_id()
    {
        let _ = chat::create_chat(conn, &id, "New chat");
    }
    seed_provider(conn);
    // Fresh-DB showcase: a small maker team (agents + workspaces + briefs + welcome chat) so
    // first boot demonstrates the loop. Before migrate_workspaces, which would otherwise give
    // the member-only specialist its own home.
    seed::seed_showcase(conn, &data_dir());
    migrate_workspaces(conn);
    link_chats_to_default_agent(conn);
    // After the web chat is linked, so `web_agent` resolves the concierge to seed.
    seed_owner_persona(conn);
    // One persistent concierge conversation (R1): keep the most recently active chat, archive
    // the rest. Also what makes the rolling-memory marker coherent (one thread, one marker).
    if let Err(err) = chat::collapse_to_primary(conn) {
        log_line(format!("bridge: collapse chats failed: {err}"));
    }
    // Workers don't survive a restart: a task still `running` at boot is orphaned (its result
    // will never arrive) and would block its lane forever. Requeue it; the scheduler relaunches
    // and an already-produced loop pass resumes at its review.
    match db::tasks::requeue_orphaned_running(conn) {
        Ok(0) => {}
        Ok(count) => log_line(format!("bridge: requeued {count} orphaned running task(s)")),
        Err(err) => log_line(format!("bridge: orphan requeue failed: {err}")),
    }
    admin::project_registry(conn, shared);
    let chats = chat::list_chats(conn).unwrap_or_default();
    let selected = chats.first().map(|chat| chat.id.clone());
    let messages = selected
        .as_deref()
        .map(|id| chat::load_thread(conn, id).unwrap_or_default())
        .unwrap_or_default();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_OPEN_CHAT,
            App::FIELD_MESSAGES,
            App::FIELD_PIPELINE_READY,
        ]),
        |app| {
            app.chats = chats;
            app.open_chat = selected;
            app.messages = messages;
            app.pipeline_ready = true;
        },
    );
    project_chat_memory(conn, shared);
    project_memory_overview(conn, shared);
}

/// First-boot bootstrap: when no endpoint exists yet, seed a `default` endpoint +
/// agent from `.env` so the chat talks to a real model out of the box. Once any
/// endpoint exists the DB is the source of record and the System Map admin owns it —
/// env never clobbers UI edits on restart. No env config → echo fallback.
fn seed_provider(conn: &rusqlite::Connection) {
    match endpoints::is_empty(conn) {
        Ok(true) => {}
        _ => return, // already configured (admin-set) — or a read error; don't touch it
    }
    let (Ok(base_url), Ok(model)) = (env("CLAW_ENDPOINT"), env("CLAW_MODEL")) else {
        return;
    };
    let api_key = env("CLAW_API_KEY").ok().filter(|key| !key.is_empty());
    if let Err(err) = endpoints::upsert(conn, "default", &base_url, api_key.as_deref(), None) {
        log_line(format!("bridge: seed endpoint failed: {err}"));
        return;
    }
    match agents::ensure_default(conn, Some("default"), Some(&model)) {
        Ok(_) => println!("Agent: native endpoint {base_url} (model {model})"),
        Err(err) => log_line(format!("bridge: seed agent failed: {err}")),
    }
}

/// Give every agent a workspace it **owns** (its home), idempotent. An agent that predates
/// the workspace model — or one inherited from a shared rust-claw central.db — gets a
/// workspace whose id is its legacy `folder`, so its existing directory and `AGENT.md` (now
/// the workspace's project brief) carry over untouched. `set_owner` also makes it a member.
/// The per-agent identity prompt starts empty (the run falls back to base + brief until the
/// operator sets one). Agents that already own a workspace are left alone.
fn migrate_workspaces(conn: &rusqlite::Connection) {
    let Ok(agents) = agents::list(conn) else {
        return;
    };
    for agent in agents {
        // Give an agent an owned home only if it belongs to no workspace at all (a legacy /
        // unattached agent). Member-only agents — e.g. a showcase specialist — keep none.
        if workspaces::is_attached(conn, &agent.id).unwrap_or(false) {
            continue;
        }
        let _ = workspaces::create(conn, &agent.folder, &agent.name);
        let _ = workspaces::set_owner(conn, &agent.folder, &agent.id);
    }
}

/// Give the concierge / workspace owner (the web agent) a default identity persona if it has
/// none, so it's never a blank agent — the diagnosed failure where the owner had no persona,
/// so it read as faceless and `create_task` auto-routing had nothing to describe it by.
/// Idempotent: only fills a missing persona, never overwrites an operator-set one.
fn seed_owner_persona(conn: &rusqlite::Connection) {
    let Ok(Some(agent_id)) = agents::web_agent(conn) else {
        return;
    };
    if db::agent_prompts::get(conn, &agent_id)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let name = agents::get(conn, &agent_id)
        .ok()
        .flatten()
        .map(|agent| agent.name)
        .unwrap_or_else(|| "claw".to_owned());
    let _ = db::agent_prompts::set(
        conn,
        &agent_id,
        &db::agent_prompts::default_owner_body(&name),
    );
}

/// Read a non-empty, trimmed env var.
pub(super) fn env(key: &str) -> Result<String, ()> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(())
}

/// Bind every web chat to the default agent group (idempotent) so a turn can resolve
/// its endpoint. Covers chats inherited from an earlier echo-only run.
fn link_chats_to_default_agent(conn: &rusqlite::Connection) {
    let Ok(Some(agent)) = agents::first(conn) else {
        return;
    };
    if let Ok(chats) = chat::list_chats(conn) {
        for summary in &chats {
            let _ = agents::link(conn, &summary.id, &agent.id);
        }
    }
}

/// Resolve `CLAW_SANDBOX` once at startup and announce the command-execution mode. If `isola` is
/// requested but isn't compiled in for this build/platform, say so and note we fall back to host
/// execution (rather than failing runs). The exec routing itself lands in a later subtask.
pub(super) fn log_sandbox_mode() {
    use crate::sandbox::SandboxMode;
    let requested = SandboxMode::resolve(env("CLAW_SANDBOX").ok().as_deref());
    match requested {
        SandboxMode::Host => {}
        SandboxMode::Isola if requested.effective() == SandboxMode::Isola => {
            log_line("sandbox: isola mode — agent commands run inside the workspace sandbox");
        }
        SandboxMode::Isola => log_line(
            "sandbox: CLAW_SANDBOX=isola requested but isola support isn't compiled in \
             (build with --features sandbox on Linux); running commands on the host",
        ),
    }
}
