# Handoff — claw-rwire (2026-07-06)

A snapshot to resume from. The plan lives in `ROADMAP.md` (now **Platform II**); the rules live in
CLAUDE.md; durable facts live in this session's memory. This file is just the operational state +
the hard-won gotchas.

## Where things stand

- **Platform I (P0–P3) is shipped and live-verified**: isola sandboxing, first-class projects +
  derived kinds, per-repo CI with fail-fast, the same-origin preview proxy + base-path, and the
  rwire-app template. Detail is in git history; condensed in `ROADMAP.md` → *Shipped foundation*.
- **Platform II is planned, not built.** `ROADMAP.md` was rewritten (2026-07-06) to the decided
  UX & CD plan: **P4** (unified lazily-loaded thread · agent-owned root `pipeline.yaml` with an
  enforced format·test·build floor · persisted CI/CD logs) and **P5** (lean `develop → main` with a
  Release action, both branches previewable · project-page origin + admin SSH-key store + sync
  status · the project page as CI/CD control center). Nothing in P4/P5 has been implemented yet.
- **Design source:** the UX & CD proposal artifact (2026-07-06) — the from→to for every move, with
  the locked decisions. Regenerate/refresh it with the Artifact tool if it needs updating.

## Repo state

- **claw** (`/home/artefact/claw-rwire`), branch `main`. Gates (must pass before any subtask is
  done): `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test`.
- **rwire** (`/home/artefact/rwire`), branch `main`, pushed to public `github.com/arte-fact/rwire`
  (the rwire-app template's git dep tracks it; base_path/proxy work is live there). rwire changes
  stay generic — gate workspace-wide + commit in rwire before claw adopts.

## The live instance

- **URL** https://llm.raptors.pizza · **login** see `.env` (form).
- **Binary** RELEASE (`./target/release/claw-rwire`), bound `:8080`, data dir `./claw-data`.
- **Sandbox** `CLAW_SANDBOX=isola` (on). Lanes `claw-<ws>` / `claw-<ws>-<req8>`, envs
  `rust,git,nodejs`, ~22 s snapshot restore. Preview pool ports **7900–7920**.
- **Security (in force):** `.env` holds a real OpenRouter key — never print it. `./claw-data` is
  **production** — never restart with tasks running (`SELECT COUNT(*) FROM tasks WHERE
  status='running'` must be 0). No claw-specific code in rwire.

**Safe redeploy** (after a code change; guards running tasks, sweeps preview orphans that ignore
SIGTERM as pid-1-of-namespace):
```bash
cd /home/artefact/claw-rwire
[ "$(sqlite3 claw-data/central.db "SELECT COUNT(*) FROM tasks WHERE status='running'")" = 0 ] || { echo HALT; exit 1; }
cargo build --release
old=$(ss -ltnp | grep ':8080 ' | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2); kill "$old"
until ! ss -ltn | grep -q ':8080 '; do sleep 0.5; done
for p in $(seq 7900 7920); do pid=$(ss -ltnp | grep ":$p " | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2); [ -n "$pid" ] && kill -9 "$pid"; done
setsid nohup ./target/release/claw-rwire >> claw-data/server.log 2>&1 &
sleep 2; curl -s -o /dev/null -w 'health: %{http_code}\n' http://127.0.0.1:8080/health
```

## Driving the UI (for live verification)

Use **Playwright** (real chromium), not the mcp headless browser (it drops WS writes). Needs
`LD_LIBRARY_PATH=/home/artefact/.local/lib`. Login: fill `input[name=username]` (from `.env`),
`input[name=password]` (from `.env`), click `button`, wait for `textarea`. An authenticated
`ctx.request.get(url)` carries the session cookie (proxy/`/preview/` checks). Dashboard is
`/dashboard`. Screenshot script from this session: `scratchpad/shots.py` (mobile 390 + desktop 1280).

## Architecture pointers (the seams Platform II builds on)

- **Conversation:** `ui/chat.rs` (whole-list today) + `ui/agent.rs` (`load_older` stub); shared atom
  `ui/thread.rs::entry`/`scroll` — P4a unifies the callers + adds a cursor.
- **Pipeline/kind:** `kinds::detect` drives checks/preview/prompt/badge; resolution seams
  `ci::checks_for` + `preview::command_for` (both override-then-kind) — P4b slots the yaml here.
- **CI:** `src/ci.rs` runs per-repo checks, keeps only a 2 000-char red tail (discards the rest);
  the macro gate holds on red at `bridge/requests/control.rs:256` — P4b hardens it into the floor.
- **Preview:** `bridge/preview.rs` in-memory supervisor, `/preview/<key>/` proxy (registered in
  `main.rs`), `develop`-staging via `refresh_staging` — P5a adds a `main` instance.
- **Branches:** `main` frozen; `DEVELOP` const + `checkout develop && merge --no-ff` in the single
  canonical tree; push hardcoded `origin develop <branch>` — P5a/P5b generalize these.
- **Project page:** `ui/project_page.rs` static read-model; `ProjectDetail` (`state/views.rs:807`)
  has no branch/checks fields — P5c makes it the control center.

## Gotchas (hard-won)

- **Requests can't be triggered from the host.** `orchestrate::ask` → `launch_planning` spawns an
  **in-process** thread; there's no DB-poke that starts one. Trigger via the WS concierge (Playwright).
- **`MAX_ACTIVE=2`** (`db/requests.rs`): only the 2 oldest active requests get lanes (FIFO); a
  non-terminal request holds its lane forever. Abandon/deliver to free one.
- **First preview in a fresh rwire-app workspace pays a full framework compile** (git-fetch + build
  rwire, minutes). The normal producer flow warms it via CI first; warm with `cargo build` if you
  preview directly.
- **The git dep needs the GitHub repo public** (the sandbox has no creds). It is public now; keep it so.
- **isola previews reparent to pid-1-of-namespace** and ignore host SIGTERM — reap with a `/proc`
  sweep + SIGKILL on the pool port (the redeploy snippet above does this).
- **Push auth (Platform II P5b):** the sandbox cuts off https credential helpers; harness git runs
  host-side. A project-page origin that actually pushes needs an ssh remote + a key from the admin
  store — https won't authenticate in-lane.
