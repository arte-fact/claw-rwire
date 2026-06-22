# Fresh-start showcase — design

> On a **fresh DB**, first boot seeds a small, real **maker/software team** so the app's
> headline (concierge → team → workspaces → task board) is legible immediately — **without**
> any fake task cards or auto-executed work. The board stays empty until the user delegates;
> the welcome chat shows them how. Decisions: *structure-only*, *maker/software team* (asked).

## What gets seeded (fresh DB only)

**Gate:** only when the DB is fresh — exactly the default agent, no workspaces yet — and not
disabled by `CLAW_SEED_DEMO=0`. Never touches an existing install.

**Agents (3)** — endpoint + model copied from the seeded `claw` (so they run once a provider
is configured; in echo mode they're inert but visible):

| Agent | Role | Persona (identity layer) | Workspaces |
|---|---|---|---|
| **claw** (`ag-default`) | concierge + lead | generalist (existing auto-seed) | **owns Studio**, member of Research |
| **Forge** | engineer | "pragmatic engineer; small, tested changes; follows conventions" | member of Studio |
| **Scout** | researcher | "sharp researcher; sources, comparisons, clear summaries" | **owns Research**, member of Studio |

Owner is unique per agent → two owned workspaces need two owners (claw, Scout); Forge is
**member-only** (demonstrates an agent with no home). No `agent_tools` rows — task executors
default to the workspace file tools, so Forge/Scout can write files out of the box.

**Workspaces (2)** with real `AGENT.md` briefs (purpose + the `projects/`/`docs/`/`skills/`
conventions), dirs scaffolded up front:
- **Studio** — "where we build things" (owner claw; members claw, Forge, Scout).
- **Research** — "where we gather, compare, summarize" (owner Scout; members Scout, claw).

→ makes the **dashboard all-lanes view**, **Team** tabs, the **persona enum in `create_task`**,
and the **Prompts inspector** (3 agents × 3 run-kinds) all meaningful on first boot.

**Welcome chat** — one concierge message in the seed thread: who the team is, how delegation
works, a concrete first prompt to try, and where to look (Dashboard / a workspace's Tasks,
Files, Team / Prompts). The entry point to the live loop.

**No task cards.** The board is empty until the user's first delegation — authentic, no
side effects, works with or without a model.

## Implementation notes

- New `bridge/seed.rs::seed_showcase(conn)`, called in `boot` **after `seed_provider`**
  (needs `ag-default` + the endpoint) and **before `migrate_workspaces`**.
- `migrate_workspaces` refined: give an agent a home only if it's a member of **no** workspace
  (was: owns none). So Forge (member of Studio) doesn't get an auto "Forge" home; legacy
  unattached agents still do. Safe for upgrades (a legacy agent owns+belongs or neither).
- Briefs: scaffold each workspace dir, then write the custom `AGENT.md` (scaffold won't clobber
  an existing one).
- Idempotent via the freshness gate; re-boot is a no-op.

## Out of scope
- Seeded/auto-run task cards (rejected: side effects / fake data).
- A scripted "run the demo" button (could come later, behind a configured endpoint).
