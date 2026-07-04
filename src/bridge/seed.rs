//! Fresh-DB showcase seed: a small maker/software team — agents, workspaces, briefs, and a
//! welcome chat — so first boot demonstrates the concierge → team → task-board loop. Structure
//! only: no task cards, no auto-run work. Gated to a truly fresh DB; never touches an existing
//! install, and skipped with `CLAW_SEED_DEMO=0`. See `docs/fresh-start-showcase.md`.

use std::path::Path;

use crate::db::pipeline::{self, StepFields};
use crate::db::{agent_prompts, agent_tools, agents, chat, workspaces};
use crate::state::MessageRole;
use crate::tools::WORKSPACE_TOOLS;
use crate::workspace;

const FORGE_ID: &str = "ag-forge";
const SCOUT_ID: &str = "ag-scout";
const CRITIC_ID: &str = "ag-critic";
const STUDIO: &str = "studio";
const RESEARCH: &str = "research";

/// The web MCP tool names a researcher needs. Only realised when the web server is connected; left
/// in the agent's selection so they light up automatically once it is.
const WEB_TOOLS: [&str; 4] = ["search", "fetch", "screenshot", "interact"];

const FORGE_PERSONA: &str = "You are Forge, a pragmatic software engineer. You write clean, \
idiomatic, well-tested code, make small focused changes, and follow the workspace's \
conventions. You implement and verify — you don't just describe what to do, and you never report \
something done without actually running it: build it and run its tests first. You keep your \
changes scoped to the task at hand and leave other tasks' files alone.";

const SCOUT_PERSONA: &str = "You are Scout, a sharp researcher. You gather primary sources, \
compare options on the dimensions that matter, and write clear, well-organized summaries. You \
note where claims come from and flag uncertainty.";

const CRITIC_PERSONA: &str = "You are Critic, a meticulous reviewer. You check work against its \
spec for correctness, completeness, and quality, and give specific, actionable feedback. You're \
fair but exacting — approve work that's genuinely done, and send back work that isn't. You record \
every decision by calling the `verdict` tool, and you never approve code without evidence that it \
builds and its tests pass.";

/// Role-preamble for Studio's first loop step (**Build**). It is the *sole* preamble for that
/// producer (a step's `prompt` replaces the role default, it is not layered), so it must carry the
/// full executor guidance. Folds the old `Execute` stage into iteration 0: the first pass builds
/// from the spec; later passes address the reviewer's feedback.
const BUILD_PROMPT: &str = "You are building this task inside the workspace, with real file tools \
(`write`/`edit`/`read`/`bash`). On the first pass, implement it end to end from the spec; on a \
later pass you'll be given a reviewer's feedback — address exactly what they flagged while keeping \
what already works. Keep this project's files together under `projects/<project-name>/` (e.g. \
`projects/foo/src/lib.rs`) — paths resolve from the workspace root, so never write `src/…` at the \
root even if the spec names a bare path. Either way: make the project build and its tests pass \
before you finish (run them — don't assume), and keep your changes scoped to this task's files. \
Briefly note what you did and confirm it builds and tests clean. Call tools using the provided \
tool-call mechanism, never by writing the call out as text.";

/// Role-preamble for Research's sole loop step (**Research**). Self-contained (a step `prompt`
/// replaces the role default, it is not layered). Written to defeat the observed failure: a weak
/// model spent all its tool rounds searching and returned the survey as chat prose, never calling
/// `write` — so the report never became a file. Hence the explicit order and the "the file is the
/// only deliverable" framing.
const RESEARCH_PROMPT: &str = "You are researching this task inside the workspace. Work in this \
order, and do not skip step 2:\n\
1. Gather a handful of primary sources with `search`/`fetch` — enough to answer the question well, \
not every source you can find. If the web tools return an error, say so plainly and stop rather \
than inventing findings.\n\
2. `write` the full report to a workspace-relative path under `docs/` (e.g. `docs/<topic>.md`). \
This file is your ONLY deliverable — a write-up you leave in your reply text instead of the file \
does not count and will be sent back for revision.\n\
3. `read` the file back to confirm it's really there.\n\
You have a limited number of tool rounds, so write the file early (after a few searches) and \
refine it — don't spend every round searching or you'll run out before you save it. Cite where \
each claim comes from and flag uncertainty. On a later pass you'll get the reviewer's feedback — if \
they say the file is missing or empty, your very first action must be to `write` it. Call tools \
using the provided tool-call mechanism, never by writing the call out as text.";

/// Validator preamble for Research's loop: Critic reviewing Scout's write-up (a research analogue
/// of the Studio code review). Guards the failure the seed's single stage let through — a report
/// that was never written, or was hollow/unsourced, being accepted as done.
const RESEARCH_REVIEW_PROMPT: &str = "You are reviewing a research write-up against its task. `read` \
the report the researcher produced under `docs/` at its real workspace-relative path; if a read \
fails, fix the path and try again rather than guessing or giving up. Check that the file actually \
exists and isn't empty, that it answers the question asked, that it cites where its claims come \
from, and that it flags uncertainty — a missing file, an empty file, or unsourced claims is a \
`revise` (with concrete feedback on what to add), not a `pass`. Record your decision by calling the \
`verdict` tool exactly once: `pass`, `revise`, or `reject`. State your decision through the tool, \
never only in prose. Don't rewrite the report yourself.";

const STUDIO_BRIEF: &str = "# Studio\n\nWhere we build things — apps, scripts, and tooling. \
Forge does the engineering here; ask for something concrete and it gets made.\n\n## \
Conventions\n\nKeep the working tree tidy and put new files where they belong:\n\n- \
`projects/` — cloned repositories and code projects\n- `docs/` — documentation, write-ups, \
and reports\n- `skills/` — reusable guides, snippets, and how-tos\n\nEach code project lives in \
its own folder under `projects/<name>/`: put its source there (e.g. \
`projects/foo/src/lib.rs`), never at the workspace root. Tool paths resolve from the workspace \
root, so always reference files by their full workspace-relative path.\n";

const RESEARCH_BRIEF: &str = "# Research\n\nWhere we gather, compare, and summarize. Scout owns \
this — point it at a question and it produces a clear, sourced write-up in `docs/`.\n\n## \
Conventions\n\n- `projects/` — datasets or scraped material\n- `docs/` — summaries, \
comparisons, and reports (the main output)\n- `skills/` — reusable research templates\n\nWrite \
each report to `docs/<topic>.md` using a workspace-relative path, cite your sources, and don't \
report a task done until the file is actually written and reads back.\n";

const WELCOME: &str = "👋 I'm **claw**, your concierge. I don't just chat — I coordinate a \
small team across workspaces, each with its own review **workflow**.\n\n**Your team**\n- \
**Forge** — engineer, in the **Studio** workspace (apps, scripts, tooling)\n- **Critic** — \
reviewer, checks Forge's work in Studio\n- **Scout** — researcher, owns the **Research** \
workspace (gathers, compares, summarizes)\n\n**How it works:** ask me for something multi-step \
and I'll delegate it. In **Studio**, every task flows through a pipeline — in the **Build** step \
Forge writes the code and Critic reviews it until it passes, then it's done. \
**Research** works the same way: Scout writes the report and Critic checks it's sourced and \
complete before it's done. Watch it move across the board.\n\n**Try one:**\n> Build a small CLI \
tool in Studio that prints a motivational quote.\n\nThen open Studio's **Tasks** tab (the kanban board) — a card that needs you shows its \
review actions right there — and the **Workflow** tab (the pipeline — edit it, e.g. add a human \
sign-off gate). **Prompts** shows exactly what each agent is told.";

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

    // Least-privilege tool permissions so each agent can actually do its job. (An agent left
    // unconfigured silently falls back to *all* tools — explicit grants are both safer and clearer.)
    // Forge builds → the coder tools (bash/read/list/find/write/edit). Critic reviews → `read` +
    // `list` + `find` (browse and search the tree without shelling out) + `bash` (run the project's
    // build/tests); its validator turn also gets `verdict`, added by the engine. No `write`/`edit`,
    // so it verifies without rewriting the deliverable. Scout researches + writes → coder tools plus
    // the web tools.
    grant_tools(conn, FORGE_ID, &WORKSPACE_TOOLS);
    grant_tools(conn, CRITIC_ID, &["read", "list", "find", "bash"]);
    let scout_tools: Vec<&str> = WORKSPACE_TOOLS.iter().copied().chain(WEB_TOOLS).collect();
    grant_tools(conn, SCOUT_ID, &scout_tools);

    // Workspaces + ownership (set_owner also adds the owner as a member) + membership.
    let _ = workspaces::create(conn, STUDIO, "Studio");
    let _ = workspaces::create(conn, RESEARCH, "Research");
    let _ = workspaces::set_owner(conn, STUDIO, &lead.id);
    let _ = workspaces::set_owner(conn, RESEARCH, SCOUT_ID);
    let _ = workspaces::add_member(conn, FORGE_ID, STUDIO);
    let _ = workspaces::add_member(conn, CRITIC_ID, STUDIO);
    let _ = workspaces::add_member(conn, SCOUT_ID, STUDIO);
    let _ = workspaces::add_member(conn, &lead.id, RESEARCH);
    // Critic reviews Research too — the workspace-crossing reviewer.
    let _ = workspaces::add_member(conn, CRITIC_ID, RESEARCH);

    write_brief(data_dir, STUDIO, "Studio", STUDIO_BRIEF);
    write_brief(data_dir, RESEARCH, "Research", RESEARCH_BRIEF);

    // Studio's showcase pipeline — a single reviewed loop, visible on first boot:
    //   Build (loop: Forge ⇄ Critic, ≤2) → done.
    // A separate human **Approve** step used to follow, but it cost a full Forge "sign-off" run
    // (~10 tool calls re-reading + re-summarizing what Build already produced) just to tee up a
    // one-click human approval — the DB showed it added ~2 runs per task and rendered no real
    // verification, so it's dropped: Build's agent review is the gate, matching Research's lean
    // single-loop shape. (Add a human gate back per-workspace in the Workflow editor if wanted.)
    let _ = pipeline::insert(
        conn,
        "ps-studio-0",
        STUDIO,
        0,
        &StepFields {
            kind: "loop",
            name: "Build",
            agent: Some(FORGE_ID),
            prompt: Some(BUILD_PROMPT),
            validator_kind: Some("agent"),
            validator_agent: Some(CRITIC_ID),
            validator_prompt: None,
            // Four produce⇄review rounds before escalating to a human: two proved too tight
            // for real work — a live E-task burned both on a single mechanical nit and parked.
            max_iterations: 4,
            on_exhaust: "human",
        },
    );

    // Research's reviewed loop — one step: Scout writes the report, Critic checks it exists and is
    // sourced (≤4), then it completes. This closes the failure where a single unreviewed Scout run
    // was accepted as done even when it wrote nothing. On exhaust it parks for a human.
    let _ = pipeline::insert(
        conn,
        "ps-research-0",
        RESEARCH,
        0,
        &StepFields {
            kind: "loop",
            name: "Research",
            agent: Some(SCOUT_ID),
            prompt: Some(RESEARCH_PROMPT),
            validator_kind: Some("agent"),
            validator_agent: Some(CRITIC_ID),
            validator_prompt: Some(RESEARCH_REVIEW_PROMPT),
            max_iterations: 4,
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

/// Enable exactly `tools` for an agent (each as an `enabled` row), marking it configured so the
/// run offers precisely this set rather than falling back to every tool.
fn grant_tools(conn: &rusqlite::Connection, agent_id: &str, tools: &[&str]) {
    let catalog: Vec<(String, bool)> = tools.iter().map(|t| ((*t).to_owned(), true)).collect();
    let _ = agent_tools::set_tools(conn, agent_id, &catalog);
}

/// Scaffold the workspace dir (standard folders) and write its showcase `AGENT.md` brief,
/// overwriting the default scaffold seeds.
fn write_brief(data_dir: &Path, id: &str, name: &str, brief: &str) {
    let dir = workspace::dir_for(data_dir, id);
    let _ = workspace::scaffold(&dir, name);
    let _ = std::fs::write(dir.join("AGENT.md"), brief);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::pipeline::StepKind;
    use crate::db::{agents, open_in_memory, pipeline};

    /// A fresh DB needs exactly one agent and no workspaces for the showcase to seed.
    fn fresh_db_with_one_agent() -> rusqlite::Connection {
        let conn = open_in_memory().expect("db");
        agents::create(&conn, "ag-lead", "claw", None, None).expect("lead agent");
        conn
    }

    #[test]
    fn studio_pipeline_is_a_single_build_loop() {
        let conn = fresh_db_with_one_agent();
        let data = tempfile::tempdir().expect("tmp");
        seed_showcase(&conn, data.path());

        let steps = pipeline::steps_for(&conn, STUDIO).expect("steps");
        // One reviewed loop: the standalone Execute stage folds into Build's iteration 0, and the
        // human Approve step is dropped (it added ~2 runs/task for no real verification).
        assert_eq!(steps.len(), 1, "Studio should have exactly the Build loop");
        assert!(
            !steps
                .iter()
                .any(|s| s.name == "Execute" || s.name == "Approve"),
            "no Execute stage and no Approve step: {:?}",
            steps.iter().map(|s| &s.name).collect::<Vec<_>>()
        );

        let build = &steps[0];
        assert_eq!(build.name, "Build");
        assert_eq!(build.kind, StepKind::Loop);
        assert_eq!(build.validator_kind.as_deref(), Some("agent"));
        assert_eq!(build.validator_agent.as_deref(), Some(CRITIC_ID));
    }

    #[test]
    fn agents_are_seeded_with_role_appropriate_tools() {
        let conn = fresh_db_with_one_agent();
        let data = tempfile::tempdir().expect("tmp");
        seed_showcase(&conn, data.path());

        // Forge builds → the coder tools, no web (selection comes back sorted).
        let forge = agent_tools::selection(&conn, FORGE_ID)
            .expect("sel")
            .expect("configured");
        assert_eq!(forge, vec!["bash", "edit", "find", "list", "read", "write"]);

        // Critic reviews → `read` + `list` + `find` (browse/search without shelling out) + `bash`
        // (build/tests), but never `write`/`edit` (it verifies, it doesn't rewrite the deliverable).
        let critic = agent_tools::selection(&conn, CRITIC_ID)
            .expect("sel")
            .expect("configured");
        assert_eq!(critic, vec!["bash", "find", "list", "read"]);
        assert!(!critic.iter().any(|t| t == "write" || t == "edit"));

        // Scout researches + writes → coder tools plus the web tools.
        let scout = agent_tools::selection(&conn, SCOUT_ID)
            .expect("sel")
            .expect("configured");
        for needed in ["read", "write", "edit", "search", "fetch"] {
            assert!(scout.contains(&needed.to_owned()), "scout missing {needed}");
        }
        // None of the workers fall back to the implicit "all tools" default.
        assert!(!forge.contains(&"search".to_owned()));
    }

    #[test]
    fn research_is_a_reviewed_loop_with_critic() {
        let conn = fresh_db_with_one_agent();
        let data = tempfile::tempdir().expect("tmp");
        seed_showcase(&conn, data.path());

        // Research now has a review gate (was an unreviewed single stage): one loop step, Scout
        // produces, Critic validates.
        let steps = pipeline::steps_for(&conn, RESEARCH).expect("steps");
        assert_eq!(
            steps.len(),
            1,
            "Research should have exactly the Research loop"
        );
        let research = &steps[0];
        assert_eq!(research.name, "Research");
        assert_eq!(research.kind, StepKind::Loop);
        assert_eq!(research.agent.as_deref(), Some(SCOUT_ID));
        assert_eq!(research.validator_kind.as_deref(), Some("agent"));
        assert_eq!(research.validator_agent.as_deref(), Some(CRITIC_ID));

        // Critic must be a member of Research to review there.
        let members = workspaces::members_of(&conn, RESEARCH).expect("members");
        assert!(
            members.iter().any(|m| m == CRITIC_ID),
            "Critic reviews Research: {members:?}"
        );
    }

    #[test]
    fn build_prompt_scopes_to_projects() {
        // The Build step prompt replaces the role default, so the project-path guidance has to
        // live here too (else the workspace-root-scatter fix never reaches Forge).
        assert!(
            BUILD_PROMPT.contains("projects/"),
            "Build must scope to projects/"
        );
    }

    #[test]
    fn research_prompts_demand_a_written_sourced_report() {
        // Producer: write to docs/ and read it back (guards the "claimed done, wrote nothing" run).
        let produce = RESEARCH_PROMPT.to_lowercase();
        assert!(produce.contains("docs/"));
        assert!(produce.contains("read it back") || produce.contains("read"));
        // Validator: a missing/hollow report is a revise, recorded via the verdict tool.
        let review = RESEARCH_REVIEW_PROMPT.to_lowercase();
        assert!(review.contains("verdict"));
        assert!(review.contains("revise"));
    }
}
