# rwire component adoption — refactor plan

Goal: replace claw's hand-rolled `el(El::*)` UI with rwire's first-class components (per CLAUDE.md
"migrate toward first-class components"). The rwire **design system** (`apps/rwire-design-system`,
served on `:9002`) catalogs **50 components** in 6 groups; the ones claw should adopt:

## Component map (claw pattern → rwire component)

| claw hand-rolls today | rwire component | API |
|---|---|---|
| `el(El::Button)…on(Ev::Click,h)` (~50×) | **`Button`** | `Button::primary/secondary/ghost/destructive(text)` or `icon_only(Icon,label)`, `.size(ButtonSize)`, `.disabled/.loading/.full_width`, then `.on_click(h)` → `ElementBuilder`, then `.data(k,v)` |
| raw `el(El::Input)` + `.attr("type"/"name"/…)` | **`Input`** | `Input::text/email/password/number/search()`, `.placeholder().value().name().id().size()` |
| raw `el(El::Textarea)` | **`Textarea`** | similar builder |
| raw `el(El::Select)`+`el(El::Option)` | **`Select`** | `Select::new().name(n).value(selected).option(v, l)…` — `.option()` takes **two args** (value, label), not a `SelectOption`; `.value()` marks the selected one |
| `labeled(label, field)` wrapper (workspace_workflow) | **`FormField`** | `FormField::new().label(l).input(el).help().error().required()` |
| custom `card(label, body)` (workspace.rs) | **`Card`** | `Card::new().padding().bordered().children([…])` (+ a label row) |
| `el(El::Div/Span).st([TextSm,TextMuted,…]).text(…)` (pervasive) | **`Text`** | `Text::body/body_small/heading1-3/label/caption(s)` + `.muted()/.accent()/.success()/.warning()/.error()` or `.color(TextColor::High)` |
| flex-row/col `el(El::Div).st([DisplayFlex,…])` | **`Stack`** | `Stack::row()/column().gap(Gap::*).align().justify().children([…])` |
| custom `status_badge` / `verdict_chip` | **`Badge`** / **`Tag`** | `Badge::success/warning/error(text)`, `Tag::new(text).intent()` |
| custom tab chips (Peek modal) | **`Tabs`** | `Tabs::new().tab(Tab::new(label,content)).active(i)` |
| custom run/tool collapse (run_chain) | **`Accordion`** | `Accordion::new().item(AccordionItem::new(t).open(b).on_toggle(h))` |
| "No tasks yet." / empty messages | **`EmptyState`** | `EmptyState::…` |
| locked/notice banners | **`Alert`** | `Alert::new().intent(AlertIntent::*)` |
| live "Running" text | **`Spinner`** | `Spinner::new().size(SpinnerSize::Sm)` |

Already used well: `Avatar`, `Tooltip`, `Tag`, `Drawer` (new-task). Keep.

## Per-screen targets (by raw-element count)

| screen | raw els | adopt |
|---|---|---|
| `workspace_tasks.rs` | 22 | Button (toolbar/filters/card actions), Input/Textarea/Select (create+edit forms), EmptyState |
| `workspace_workflow.rs` | 12 | FormField+Input/Select/Textarea (step form), Button (add/move/delete/save), Alert (lock notice) |
| `system_map/agents.rs` | 8 | Button, Input/Select |
| `prompts.rs` | 8 | Button, Textarea, Select |
| `workspace.rs` | 7 | Card (Team/Overview), Input/Select (name/owner), Button (delete), Textarea (brief) |
| `task_detail.rs` | 6 | Button (controls/gate/close), Tabs (peek tabs), Spinner (live) |
| `review.rs` | 5 | Button (approve/reject/changes/open), Input (feedback), Card |
| `system_map/mod.rs`, `chat.rs`*, `files.rs`*, `run_chain.rs`, `dashboard.rs`, `mod.rs`, `widgets.rs`* | 1–4 | Button, Input, Accordion |

\* `chat.rs`/`files.rs`/`widgets.rs` are mid token-migration (uncommitted, owner's) — coordinate before touching.

## Constraints / gotchas (learned this session)

- **`Modal`** has a backdrop-z bug (dims content) and **`Drawer`** had a transient open/close issue —
  both flagged in `HANDOFF-from-claw.md`. claw's Peek modal + step editor use custom overlays for now;
  migrate to `Modal`/`Drawer` **after** those land in rwire.
- Buttons that carry `data-*` for handlers: `Button::…on_click(h)` returns `ElementBuilder`, so chain
  `.data("task", id)` after it.
- `FontInheritAll`/`HDvh`/`Button::icon` exist only in the owner's **uncommitted** rwire edits — refactors
  using them need that committed first (rust-analyzer shows phantom "not found"; `cargo build` is clean).
- `Input`/`Textarea` `.autocomplete()`/`.spellcheck()` are also **uncommitted** — stay on committed setters
  (`.name().value().placeholder().rows()`); drop `autocomplete="off"` rather than depend on them.
- `Button` has **no submit-type setter** and `icon_only` is uncommitted — leave `<button type=submit>` and
  icon-only buttons as raw `el(El::Button)` for now.
- Components apply their own `font:inherit` + base styling, so drop the manual `.style("font","inherit")`
  and the `field_st()` token bundle when swapping to `Input`/`Textarea`/`Select`.

## Approach

One screen per PR, each landing green on `cargo fmt && clippy -D warnings && cargo test` + a live
screenshot. Order by leverage: shared `widgets.rs` helpers first (propagate everywhere) once the owner's
migration lands, then form-heavy screens (`workspace_workflow`, `workspace`, `prompts`), then the rest.

## Codebase reduction (running tally)

Component adoption is net-negative — the components absorb the styling/attrs claw spelled out by hand.

| screen | net lines |
|---|---|
| review + workflow | **−105** |
| workspace (Card/Input/Select) | −34 |
| workspace (Text) | −10 |
| dashboard | −10 |
| prompts | −46 |
| system_map/agents | −28 |
| system_map/mod (shared helpers) | −35 |
| **total (7 screens)** | **−268** (+163 / −431) |

Trend: ~−40 lines/screen on form-heavy screens, less on text-only ones. Remaining screens
(`workspace_tasks` is the big one) should keep the slope.

## Progress checklist

Tick a screen only when it lands green on the gate **and** is visually re-checked. A screen may need
several sub-swaps (Button / Input / Select / Card / …); list them so partial progress is visible.

- [ ] **`widgets.rs`** (shared helpers — highest leverage) — `accent_submit`/`icon_button`/`action`/`action_icon` → `Button`. _Blocked: owner's uncommitted token migration; coordinate first._
- [x] **`workspace_workflow.rs`** — [x] `Alert` (lock notices) · [x] `Button` (add/move/delete via `icon_btn`/`add_btn`) · [x] `Input`/`Textarea`/`Select` (step form fields, pre-selection confirmed). [ ] submit + ✕-close stay raw (Button has no submit-type method; `icon_only` is uncommitted) · [ ] `labeled`→`FormField` deferred. _Gate-green (fmt/clippy/168 tests) **+ visually verified** (step editor renders all fields with correct pre-filled/selected values)._
- [~] **`workspace.rs`** — [x] `Card` (all Team/Overview blocks via `card()`) · [x] `Input` (name) · [x] `Select` (owner, pre-selection confirmed) · [x] `Text` (roster/hints/empties/errors via `Text::body`/`caption`). [ ] member toggle-chips + auto-approve toggle stay raw (state-styled buttons) · [ ] brief editor stays raw (custom `FontMono` + min-height) · [ ] delete uses shared `widgets::action` (migrate with widgets.rs). _Gate-green + visually verified._
- [~] **`prompts.rs`** — [x] `Select` (agent picker) · [x] `Textarea` (role + memory editors) · [x] `Button` (`plain_button`). [ ] run-kind chips stay raw (toggle-state) · [ ] `text_block` stays raw (custom pre-wrap/word-break) · [ ] `Text` for headings/empties/context deferred. _Gate-green._
- [~] **`system_map/agents.rs`** — [x] `Select` (endpoint + model) · [x] `Textarea` (persona). [ ] tool-chips stay raw (toggle-state) · [ ] `action`/`badge`/`text_input` are shared `system_map` helpers (migrate in `system_map/mod.rs`). _Gate-green._
- [x] **`system_map/mod.rs`** — [x] `Input` (shared `text_input`/`field` → propagates to agents + endpoints tiers) · [x] `Select` (channel picker) · [x] `Alert` (error banner); drops `select_st()`. Already had `Text`/`Drawer`/`action`/`submit`. _Gate-green._
- [ ] **`workspace_tasks.rs`** — toolbar/filter/card-action `Button` · create+edit `Input`/`Textarea`/`Select` · `EmptyState` ("No tasks yet") _(largest — 22 raw els)_
- [ ] **`task_detail.rs`** — `Button` (controls/gate/close) · `Tabs` (Peek tabs) · `Spinner` (live). _Defer Peek `Modal` until the rwire Modal backdrop fix lands._
- [~] **`review.rs`** — [x] buttons → `Button` (approve/reject/open). [ ] feedback field → `Input`; [ ] `card()` → `Card`; [ ] submit chip → `Button`
- [ ] **`run_chain.rs`** — tool/run collapse → `Accordion`
- [~] **`dashboard.rs`** — [x] `Input` (create form) · [x] `Text` (name/owner/empty/needs-you). [ ] lane line stays raw (dynamic color incl. `TextLow`, no `Text` variant). _Gate-green._
- [ ] **`mod.rs`** (shell nav) — `Button` where applicable
- [ ] _coordinate before touching:_ **`chat.rs`**, **`files.rs`**, **`widgets.rs`** — mid token-migration (owner's uncommitted work)

**Cross-cutting (do once the components are adopted):**
- [ ] Migrate Peek modal + step editor from custom overlays → `Modal` / `Drawer` (after the rwire `Modal` backdrop-z + `Drawer` fixes land — see `HANDOFF-from-claw.md`)
- [ ] Replace remaining custom `status_badge`/`verdict_chip` → `Badge`/`Tag` consistently
- [ ] Replace ad-hoc flex `el(El::Div).st([DisplayFlex…])` → `Stack` where it improves readability
