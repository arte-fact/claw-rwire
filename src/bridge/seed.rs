//! Fresh-DB showcase seed: a small maker/software team — agents, workspaces, briefs, and a
//! welcome chat — so first boot demonstrates the concierge → team → task-board loop. Structure
//! only: no task cards, no auto-run work. Gated to a truly fresh DB; never touches an existing
//! install, and skipped with `CLAW_SEED_DEMO=0`. See `docs/fresh-start-showcase.md`.

use std::path::Path;

use crate::db::{agent_prompts, agents, chat, pipeline, workspaces};
use crate::state::MessageRole;
use crate::workspace;

const FORGE_ID: &str = "ag-forge";
const SCOUT_ID: &str = "ag-scout";
const STUDIO: &str = "studio";
const RESEARCH: &str = "research";

const FORGE_PERSONA: &str = "You are Forge, a pragmatic software engineer. You write clean, \
idiomatic, well-tested code, make small focused changes, and follow the workspace's \
conventions. You implement and verify — you don't just describe what to do.";

const SCOUT_PERSONA: &str = "You are Scout, a sharp researcher. You gather primary sources, \
compare options on the dimensions that matter, and write clear, well-organized summaries. You \
note where claims come from and flag uncertainty.";

/// Role-preamble override for Studio's "Refine" stage — it improves the prior step's output.
const REFINE_PROMPT: &str = "You are refining the previous step's work, not starting over. \
Read what was produced, then improve its correctness, clarity, and polish — fix issues, tighten \
the code/prose, and keep what already works. Briefly note what you changed.";

const STUDIO_BRIEF: &str = "# Studio\n\nWhere we build things — apps, scripts, and tooling. \
Forge does the engineering here; ask for something concrete and it gets made.\n\n## \
Conventions\n\nKeep the working tree tidy and put new files where they belong:\n\n- \
`projects/` — cloned repositories and code projects\n- `docs/` — documentation, write-ups, \
and reports\n- `skills/` — reusable guides, snippets, and how-tos\n";

const RESEARCH_BRIEF: &str = "# Research\n\nWhere we gather, compare, and summarize. Scout owns \
this — point it at a question and it produces a clear, sourced write-up in `docs/`.\n\n## \
Conventions\n\n- `projects/` — datasets or scraped material\n- `docs/` — summaries, \
comparisons, and reports (the main output)\n- `skills/` — reusable research templates\n";

const WELCOME: &str = "👋 I'm **claw**, your concierge. I don't just chat — I coordinate a \
small team across workspaces and a shared task board.\n\n**Your team**\n- **Forge** — \
engineer, in the **Studio** workspace (apps, scripts, tooling)\n- **Scout** — researcher, owns \
the **Research** workspace (gathers, compares, summarizes)\n\n**How it works:** ask me for \
something multi-step and I'll delegate it. The workspace owner plans it into tasks, the right \
teammate runs each one in its workspace, and I report back here when they're done.\n\n**Try \
one:**\n> Research the top 3 Rust web frameworks and write a comparison in the Research \
workspace.\n\nThen watch the **Dashboard** and a workspace's **Tasks** tab fill in. Explore \
**Prompts** to see exactly what each agent is told, and a workspace's **Files** / **Team** \
tabs.";

/// Seed the showcase if the DB is fresh (exactly the default agent, no workspaces) and the
/// demo isn't disabled. Best-effort — any step failing just leaves a smaller seed.
pub fn seed_showcase(conn: &rusqlite::Connection, data_dir: &Path) {
    if std::env::var("CLAW_SEED_DEMO").map(|v| v.trim() == "0") == Ok(true) {
        return;
    }
    let one_agent = agents::list(conn).map(|a| a.len()) == Ok(1);
    let no_workspaces = workspaces::list(conn).map(|w| w.is_empty()) == Ok(true);
    if !one_agent || !no_workspaces {
        return;
    }
    let Ok(Some(lead)) = agents::first(conn) else {
        return;
    };

    // Specialists share the lead's endpoint/model (inert until a provider is configured).
    let endpoint = lead.endpoint.as_deref();
    let model = lead.model.as_deref();
    if agents::create(conn, FORGE_ID, "Forge", endpoint, model).is_err()
        || agents::create(conn, SCOUT_ID, "Scout", endpoint, model).is_err()
    {
        return;
    }
    let _ = agent_prompts::set(conn, FORGE_ID, FORGE_PERSONA);
    let _ = agent_prompts::set(conn, SCOUT_ID, SCOUT_PERSONA);

    // Workspaces + ownership (set_owner also adds the owner as a member) + membership.
    let _ = workspaces::create(conn, STUDIO, "Studio");
    let _ = workspaces::create(conn, RESEARCH, "Research");
    let _ = workspaces::set_owner(conn, STUDIO, &lead.id);
    let _ = workspaces::set_owner(conn, RESEARCH, SCOUT_ID);
    let _ = workspaces::add_member(conn, FORGE_ID, STUDIO);
    let _ = workspaces::add_member(conn, SCOUT_ID, STUDIO);
    let _ = workspaces::add_member(conn, &lead.id, RESEARCH);

    write_brief(data_dir, STUDIO, "Studio", STUDIO_BRIEF);
    write_brief(data_dir, RESEARCH, "Research", RESEARCH_BRIEF);

    // A 2-stage pipeline for Studio so chaining is visible: Forge builds, then refines. (F-6
    // upgrades this to a validation loop with a Critic.) Research keeps the default single stage.
    let _ = pipeline::insert_stage(
        conn,
        "ps-studio-0",
        STUDIO,
        0,
        "Execute",
        Some(FORGE_ID),
        None,
    );
    let _ = pipeline::insert_stage(
        conn,
        "ps-studio-1",
        STUDIO,
        1,
        "Refine",
        Some(FORGE_ID),
        Some(REFINE_PROMPT),
    );

    // The concierge's welcome, in the seed thread.
    if let Ok(chats) = chat::list_chats(conn)
        && let Some(seed) = chats.first()
    {
        let _ = chat::append_message(conn, &seed.id, MessageRole::Agent, WELCOME);
    }
}

/// Scaffold the workspace dir (standard folders) and write its showcase `AGENT.md` brief,
/// overwriting the default scaffold seeds.
fn write_brief(data_dir: &Path, id: &str, name: &str, brief: &str) {
    let dir = workspace::dir_for(data_dir, id);
    let _ = workspace::scaffold(&dir, name);
    let _ = std::fs::write(dir.join("AGENT.md"), brief);
}
