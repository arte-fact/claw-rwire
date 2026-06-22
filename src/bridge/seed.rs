//! Fresh-DB showcase seed: a small maker/software team — agents, workspaces, briefs, and a
//! welcome chat — so first boot demonstrates the concierge → team → task-board loop. Structure
//! only: no task cards, no auto-run work. Gated to a truly fresh DB; never touches an existing
//! install, and skipped with `CLAW_SEED_DEMO=0`. See `docs/fresh-start-showcase.md`.

use std::path::Path;

use crate::db::pipeline::{self, StepFields};
use crate::db::{agent_prompts, agents, chat, workspaces};
use crate::state::MessageRole;
use crate::workspace;

const FORGE_ID: &str = "ag-forge";
const SCOUT_ID: &str = "ag-scout";
const CRITIC_ID: &str = "ag-critic";
const STUDIO: &str = "studio";
const RESEARCH: &str = "research";

const FORGE_PERSONA: &str = "You are Forge, a pragmatic software engineer. You write clean, \
idiomatic, well-tested code, make small focused changes, and follow the workspace's \
conventions. You implement and verify — you don't just describe what to do.";

const SCOUT_PERSONA: &str = "You are Scout, a sharp researcher. You gather primary sources, \
compare options on the dimensions that matter, and write clear, well-organized summaries. You \
note where claims come from and flag uncertainty.";

const CRITIC_PERSONA: &str = "You are Critic, a meticulous reviewer. You check work against its \
spec for correctness, completeness, and quality, and give specific, actionable feedback. You're \
fair but exacting — approve work that's genuinely done, and send back work that isn't.";

/// Role-preamble override for Studio's "Refine" loop — it improves the prior step's output.
const REFINE_PROMPT: &str = "You are refining the previous step's work, not starting over. \
Read what was produced, then improve its correctness, clarity, and polish — fix issues, tighten \
the code/prose, and keep what already works. Briefly note what you changed.";

/// Role-preamble override for Studio's final "Approve" loop — prepares the work for sign-off.
const FINALIZE_PROMPT: &str = "Prepare the final version for the user's sign-off. Make sure it's \
complete and correct, then briefly summarize what was built and confirm it's ready to ship.";

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
small team across workspaces, each with its own review **workflow**.\n\n**Your team**\n- \
**Forge** — engineer, in the **Studio** workspace (apps, scripts, tooling)\n- **Critic** — \
reviewer, checks Forge's work in Studio\n- **Scout** — researcher, owns the **Research** \
workspace (gathers, compares, summarizes)\n\n**How it works:** ask me for something multi-step \
and I'll delegate it. In **Studio**, every task flows through a pipeline — Forge builds, then a \
**Refine** loop has Critic review until it passes, then it parks for **your** approval. Watch it \
move across the board.\n\n**Try one:**\n> Build a small CLI tool in Studio that prints a \
motivational quote.\n\nThen open Studio's **Tasks** tab (the kanban board), **Workflow** tab (the \
pipeline — edit it!), and the **Review** tab when something needs you. **Prompts** shows exactly \
what each agent is told.";

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
        || agents::create(conn, CRITIC_ID, "Critic", endpoint, model).is_err()
    {
        return;
    }
    let _ = agent_prompts::set(conn, FORGE_ID, FORGE_PERSONA);
    let _ = agent_prompts::set(conn, SCOUT_ID, SCOUT_PERSONA);
    let _ = agent_prompts::set(conn, CRITIC_ID, CRITIC_PERSONA);

    // Workspaces + ownership (set_owner also adds the owner as a member) + membership.
    let _ = workspaces::create(conn, STUDIO, "Studio");
    let _ = workspaces::create(conn, RESEARCH, "Research");
    let _ = workspaces::set_owner(conn, STUDIO, &lead.id);
    let _ = workspaces::set_owner(conn, RESEARCH, SCOUT_ID);
    let _ = workspaces::add_member(conn, FORGE_ID, STUDIO);
    let _ = workspaces::add_member(conn, CRITIC_ID, STUDIO);
    let _ = workspaces::add_member(conn, SCOUT_ID, STUDIO);
    let _ = workspaces::add_member(conn, &lead.id, RESEARCH);

    write_brief(data_dir, STUDIO, "Studio", STUDIO_BRIEF);
    write_brief(data_dir, RESEARCH, "Research", RESEARCH_BRIEF);

    // Studio's showcase pipeline — the whole feature, visible on first boot:
    //   Execute (stage, Forge) → Refine (loop: Forge ⇄ Critic, ≤2) → Approve (loop: Forge ⇄ you).
    // Research keeps the implicit default single stage.
    let _ = pipeline::insert_stage(
        conn,
        "ps-studio-0",
        STUDIO,
        0,
        "Execute",
        Some(FORGE_ID),
        None,
    );
    let _ = pipeline::insert(
        conn,
        "ps-studio-1",
        STUDIO,
        1,
        &StepFields {
            kind: "loop",
            name: "Refine",
            agent: Some(FORGE_ID),
            prompt: Some(REFINE_PROMPT),
            validator_kind: Some("agent"),
            validator_agent: Some(CRITIC_ID),
            validator_prompt: None,
            max_iterations: 2,
            on_exhaust: "human",
        },
    );
    let _ = pipeline::insert(
        conn,
        "ps-studio-2",
        STUDIO,
        2,
        &StepFields {
            kind: "loop",
            name: "Approve",
            agent: Some(FORGE_ID),
            prompt: Some(FINALIZE_PROMPT),
            validator_kind: Some("human"),
            validator_agent: None,
            validator_prompt: None,
            max_iterations: 2,
            on_exhaust: "human",
        },
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
