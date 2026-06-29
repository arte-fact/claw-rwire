# Workspace UX overhaul — "the board is the workspace"

Goal: a **one-page workspace** where you read state at a glance and act without leaving the
board. Cut the navigation (5 tabs + 2 global screens + a separate detail route) down to one
primary board with contextual overlays. Grounded in the current code and a live end-to-end run;
inspired by Jira (cards, quick-filters, swimlanes), GitHub Projects (inline add, peek), and
GitLab (board + side detail).

## The problem today (grounded)

- **Navigation sprawl.** Per workspace: `Overview · Files · Tasks · Workflow · Team` tabs, plus
  global `Review` and `Dashboard`, plus a separate task-detail **route** `/w/:wid/tasks/:tid`.
  To approve a parked task you leave the board (→ `/review` or → the detail page) and come back.
- **Thin kanban cards.** `kanban_card` shows only title + status + one meta line (+ inline
  Approve). No spec preview, no pipeline position, no blockers, no last output. The *rich* card
  (`task_card`, with controls + edit) only exists in the **List** view — so the two views
  disagree on how much they tell you.
- **Detail is a full page.** Good content (spec, live feed, run chain, controls) but reaching it
  costs a navigation, and `Review` duplicates the same `run_card` on yet another screen.

## What we can reuse (already built)

rwire ships these as **first-class components** (confirmed in `rwire_components`) — so the overhaul
is assembly, not new infrastructure:

| Need | Component | Note |
|---|---|---|
| Wide overlay (Peek detail) | `Modal::new().open(b).on_close(h).title().content().footer()` | centered, can be wide — right for the transcript+diffs |
| Narrow side panel (forms) | `Drawer::new().position(Right).open(b)` | **fixed 320px** — keep for edit / step-edit / member forms, *not* the diff view |
| In-place tabs | `Tabs::new().tab(Tab::new(label, content)).active(i)` | inactive panels `St::DisplayNone`, no route change |
| Disclosure / collapse | `Accordion::new().item(AccordionItem::new(t).open(b).on_toggle(h))` | `<details>` is **not** in the `El` enum — use this |
| Assignee | `Avatar::new().fallback("Fo").size(Sm)` | initials chip per teammate |
| Status / signals | `Badge::new().intent(...)`, `Tag::new(text)` | pills for status + blocked/cron/retry |
| Hover hint | `Tooltip::new(text).position(...)` | CSS-only, no state — perfect for the signal icons |
| Live run | `Spinner::new().size(Sm)`, `Progress::new().value().max()` | running indicator / loop-iteration bar |

Plus the existing app code:

- `run_chain::run_card` — shared transcript renderer with **coloured `edit` diffs** (`diff::line_diff`),
  markdown output, verdict chips. Used by both detail and Review today; reuse verbatim.
- The pure-UI-state pattern: `app.editing_task: Option<String>`, `app.new_task_panel_open: bool`
  toggled by `#[handler]`s and reacted to by a `#[renderer]` — the canonical way to drive
  overlays/tabs **without a route change**.

So this is mostly **information architecture**, not new infrastructure.

---

## 1. Richer kanban cards (the glance layer)

```
┌────────────────────────────────────────┐
│ Core module — quotes.py        ● running│   title + status dot
│ ◔ Execute › Refine › Approve   iter 2   │   pipeline mini-stepper (current step bold)
│ Add/list/random CLI storing to JSON…    │   spec preview (2-line clamp)
│ 🟣 Forge   ⛔2  ⟳ 0 8 * * *   ↻ att 2    │   assignee chip · signals row
│ ───────────────────────── [Stop] [Peek] │   inline actions on hover
└────────────────────────────────────────┘
```

Add to `kanban_card`, compactly (all from `el()` + `St` tokens, no new component):

- **Pipeline mini-stepper** — the workspace's step names with the current one emphasized + the
  loop iteration. Makes "where is this in the pipeline" visible per card (today only the column
  position implies it).
- **Spec preview** — 1–2 lines, clamped (`St::WhitespacePreWrap` + a line-clamp style).
- **Assignee** — `Avatar` (initials: Forge/Critic/owner), so the *team* is visible on the board.
- **Signals row** — `Tag`/`Badge` pills with `Tooltip` hints, only when relevant: `⛔N` blocked-by
  (**red `Badge::Error` when a blocker is dead** — surfaces the B2 cascade), `⟳ cron` recurrent +
  next-run, `↻ att N`, `⏱ retry pending`, `⏳ needs you` (accent, on parked cards).
- **Hover quick-actions** — context by status: awaiting → Approve · Changes · Reject; running →
  Stop; terminal → Restart. (Today only Approve is inline on kanban cards.)
- **Card body click → Peek drawer** (§2) instead of a route nav.

Drop the Board/List split: the kanban card now carries the List card's richness, so **one view**.

## 2. The Peek panel — the one-page task view, over the board

Replace the `/w/:wid/tasks/:tid` **page** with a **`Modal`** opened over the board (board stays
visible behind). A Modal, not the `Drawer` — the 320px Drawer is too narrow for a run-chain
transcript with code diffs. New state: `app.selected_task: Option<String>` + `app.detail_tab`.

```
 board (dimmed) ──────────────┐ ┌─────────── Peek ───────────────┐
 ▸ Execute  ▸ Refine  ▸ Appr… │ │ Core module — quotes.py   ●run │
 ┌─────────┐ ┌─────────┐      │ │ 🟣Forge · Refine·2 · 00:41     │
 │ Test …  │ │ Core …  │      │ │ [Stop] [Restart] [Cancel]      │
 │ pending │ │ running │      │ │ ── Overview │ Activity │ Review │  ← in-place tabs (no route)
 └─────────┘ └─────────┘      │ │ Spec ▾  (markdown, collapsible)│
                              │ │ Blocked by: #a ✓  #b ●running  │
                              │ │ ┄┄┄┄┄┄┄ scrollable ┄┄┄┄┄┄┄┄┄┄┄ │
                              │ │ �what                         │ │
                              │ │ ▸ Produce · Forge      pass ▾  │  ← each run = disclosure
                              │ │   ▾ Review · Critic    pass    │
                              │ │     edit quotes.py             │
                              │ │     - old line   + new line    │  ← coloured diff (reused)
                              │ └────────────────────────────────┘
```

- **Header**: title · status · assignee (`Avatar`) · step/iteration · live timer · controls inline.
- **In-place tabs** (`Tabs` + `detail_tab` state, no route):
  - **Overview** — spec (markdown, inside an `Accordion` item), blockers with **live status**
    (click to peek that blocker), schedule/retry/timing, edit-in-place (reuse `edit_form`).
  - **Activity** — live feed (`Spinner` when running) on top; then the **run chain** with each run
    as a collapsed **`Accordion` item** so a long chain stays scannable; expand → the `run_card`
    transcript + diffs. Wrapped in `St::MaxH90vh + St::OverflowYAuto` so it scrolls inside the modal.
  - **Review** — the gate actions (Approve / Request-changes / Reject) **right here** when the
    task is parked. Reviewing now happens over the board, not on a separate screen.
- **Deep links still work**: hitting `/w/:wid/tasks/:tid` (e.g. from a chat notification) sets
  `selected_task` and opens the drawer over the board — same destination, no separate page.

## 3. Fold Review into the board; keep a cross-workspace triage

- **Per-workspace** review → the Peek drawer's Review tab. No screen change.
- Keep **`/review`** only as the **cross-workspace "Needs you" queue** (it genuinely spans
  workspaces); each row opens the same Peek drawer. Add a board **"Needs you (N)"** quick-filter
  so single-workspace triage never leaves the board.

## 4. Collapse the tab sprawl into the board + overlays

| Today (tab) | Proposed |
|---|---|
| **Tasks** | the board — now the **default** workspace view |
| **Workflow** (pipeline) | the board's **column headers are the pipeline**; click a header → step-edit popover/drawer (tune in situ). Show the "locked while in-flight" state as a board banner |
| **Team** | avatar row in the board header; click → member drawer |
| **Overview/brief** | collapsible header strip, or a settings drawer |
| **Files** | stays a distinct view, but reached from the board header (drawer or tab) |

Net: **one primary screen per workspace** (the board) + contextual drawers/popovers. From five
tabs to one board.

## 5. Board ergonomics (Jira / GitLab / GitHub)

- **Quick-filter chips**: `All · Mine · Needs-you · Running · Blocked · Failed` (extends the
  existing `toggle_chip` pattern + a `board_filter` state field).
- **Swimlanes** (optional): group rows **by assignee** — each teammate's lane of work, the agile-team view.
- **Inline add**: a GitHub-style "+ add" box at the top of the first column, alongside the drawer.
- **WIP count** per column (already shown); **density toggle** (compact/comfortable) now that
  cards carry more.

---

## Feasibility notes (rwire)

- Overlays: **`Modal`** for the wide Peek (transcript + diffs); **`Drawer`** (320px) for the narrow
  forms (edit, step-edit, member). Both driven by `Option<String>`/`bool` state + `#[renderer]` —
  no route changes, no JS.
- Tabs: the `Tabs`/`Tab` component (`active` index in state). Disclosure: `Accordion`/`AccordionItem`
  (`<details>` is not in the `El` enum). Assignee: `Avatar`. Status/signals: `Badge`/`Tag` +
  `Tooltip` (CSS-only hover, no state). Running: `Spinner`/`Progress`.
- Diffs: already rendered (`run_chain::edit_diff` + `diff::line_diff`); extend to whole-file
  before/after later (lever O9).
- New state fields: `selected_task: Option<String>`, `detail_tab: usize`, `board_filter`,
  `swimlane_by_assignee: bool`, `collapsed_runs` set. All cheap, all server-side `#[derive(State)]`.

## Implementation roadmap (checkable)

Convention: each item is a PR-sized change that lands green on the gate
(`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`), **then is
live-tested in the running app and visually checked with a screenshot** before it's ticked.
Screenshots are saved under `docs/ux/` and linked from the item.

### Phase 1 — Richer kanban cards (pure render; no new routes/state)
- [x] U1.1 Assignee `Avatar` (initials) on the card — `C`/`O`/`F` circles, `Tooltip` = full name
- [x] U1.2 Pipeline mini-stepper — `Execute › Refine › Approve`, current step accented + loop iter; faint on terminal
- [x] U1.3 Spec preview — 2-line clamp (`-webkit-line-clamp`)
- [x] U1.4 Signals row (`Tag`): needs-you, blocked-by `⛔N`, cron, retry-pending, attempt; suppressed on terminal cards
- [x] U1.5 Inline quick-actions by status (Approve / Stop / Restart…) — always-visible (more discoverable than hover in this framework)
- [ ] U1.6 Merge the List card's richness into the kanban card; drop the Board/List split _(deferred — kanban now carries the richness; List kept as a dense alt view)_
- [x] **U1.✓ Live-verified** on `:8911` Studio board (gate green: fmt/clippy/168 tests). Cards show avatar + stepper + spec preview + signals + actions; terminal cards clean.

### Phase 2 — Peek modal (one-page task view, over the board) ✅
- [x] U2.1 State `selected_task` + `detail_tab`; `open_peek`/`close_peek` handlers; card title opens the `Modal`
- [x] U2.2 Modal shell: header (`Avatar` + title + step/iter + status) + close `✕`. Built as a **custom overlay** (60rem × **fixed 85vh** panel, **pinned head + single scrolling body**), not the `Modal` component — the component layers its backdrop *above* the panel (dims the content); the custom one renders backdrop + panel as siblings so the panel paints on top. Backdrop-click and `✕` both close (DOM-verified: panel = 0.85 × viewport).
- [x] U2.3 Tab strip Overview / Activity / Review (`detail_tab` chips; verified switching live)
- [x] U2.4 Activity tab: live feed (running) + run chain (reuse `run_card` + diffs), scrolls in the fixed-height body
- [x] U2.4b **Two-level collapse for the run chain.** Whole runs collapse to a one-line header (role + **files-changed summary** + agent + verdict); only the **latest run** is open by default. Inside an open run, tool calls are one-line rows (caret + name + humanized summary, *not* JSON + ✓/✗), **all collapsed by default**. Everything toggles via one `tool_toggled` set (`run:` / tool keys); the run's markdown reply stays full at the bottom. Live-verified (5 runs → 5 headers, latest open; expanded run's tools all `▸`).
- [ ] U2.4c **Content diff per run / per task** — _partial._ Done: per-`edit` old→new diffs, and a per-run files-changed summary. TODO: true before→after file diffs need a baseline the transcript doesn't store (`write` keeps only new content). Plan: make each workspace a **git repo**, commit per run → per-run diff = that commit, per-task diff = the task's commit range (also dovetails with worktree isolation, O1). Cheap interim: a per-task files-changed rollup atop Activity.
- [x] U2.5 Review tab: Approve / Request-changes / Reject (reused gate actions); note when not awaiting
- [x] U2.6 Deep-link `/w/:wid/tasks/:tid` renders the board + opens the modal (bridge `load_detail` sets `selected_task`); the full-page detail route was **removed** (folded into the modal)
- [x] **U2.✓ Live-verified**: card-click and deep-link both open the modal over the board; Overview (spec+controls) and Activity (full transcript, scrollable) confirmed. Gate green (fmt/clippy/168 tests).

### Mobile optimization pass ✅
- [x] **Kanban stacks vertically on mobile** (`base FlexCol → md FlexRow + OverflowXAuto`); columns are full-width cards you scroll *down* through, instead of swiping sideways past empty columns. Desktop unchanged (live-verified at 390px and 1400px).
- [x] **Modal near-fullscreen on mobile**: container/head/body padding tightened on base, full on `md+` (`PSm→PMd`, `PMd→PLg`, `PxMd/PbMd→PxLg/PbLg`) — more content width on small screens. Title wraps; run chain + tabs fit.
- [x] Reused the existing `md` breakpoint + hamburger drawer; no new viewport detection.

### Phase 3 — Fold Review in + filters ✅
- [x] U3.1 Board quick-filter chips: **All · Needs you · Running · Blocked · Failed** (`board_filter` state, applied in both Board + List views). _("Mine" dropped — no per-human assignee in an agent team; "Blocked" = has blockers & pending/stopped._) Live-verified: "Failed" → DONE·0, only the cancelled card; column counts update.
- [x] U3.2 `/review` rows **open the same Peek modal in place** — added `peek_modal` to the review page; the run-chain link is now an "Open task →" button calling the board's `open_peek` (plan items keep their inline proposed-task list). Page renders + modal wired (smoke-tested; the open mechanism is the board's already-verified one).
- [x] **U3.✓ Live-verified** filters at 1400px; gate green (fmt/clippy/168 tests).

### Phase 4 — Collapse tabs into the board ✅ (partial — see U4.2)
- [x] U4.1 **Board is the workspace landing** — `/w/:wid` renders the board (Tasks tab, first in the row); Overview moved to `/w/:wid/overview`. `on_route` loads the lane for the bare `/w/:wid`. Live-verified: a sidebar workspace click lands on the board.
- [x] U4.2 **In-situ step editor** — a step column header opens that step's full edit form (name/kind/agent/preamble/loop fields + Move/Delete, reusing the workflow editor) in a right-side overlay over the board; a step with no row (implicit default) or a locked board links to the Workflow tab instead; Done/Closed stay plain. Plus the **"⏳ locked while in-flight" banner**. Built on the custom-overlay pattern (not the `Drawer` component — see note). Open+close live-verified.
- [x] U4.3 **Member avatars in the workspace header** (`Avatar` + name `Tooltip`) — the team at a glance (`C F S C`). Overview/Files/Workflow/Team still reachable from the tab row.
- [x] **U4.✓ Live-verified** at 1400px: bare `/w/:wid` → board; avatars in header; tabs reordered (Tasks first); Overview renders at `/overview`. Gate green (fmt/clippy/168 tests).

### Phase 5 — Team ergonomics
- [ ] U5.1 Swimlanes grouped by assignee
- [ ] U5.2 Density toggle + inline "+ add" in the first column
- [ ] **U5.✓ Live test + screenshot** → `docs/ux/05-swimlanes.png`
