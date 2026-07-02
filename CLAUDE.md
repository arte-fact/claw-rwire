# claw-rwire

Clean-room rewrite of **claw** (personal AI assistant) on the **rwire** framework, built
from scratch alongside the original `../rust-claw` (which stays untouched and running as the
behavioural reference). Web-UI-first; SMS deferred.

- **Spec & design** live in the original repo: `../rust-claw/SPEC.md` (what + contracts) and
  `../rust-claw/docs/rewrite-on-rwire.md` (how, on rwire). Read those before extending.
- **Framework**: `../rwire` (path deps). async-std runtime, synchronous `#[handler]` /
  `#[renderer]`, `#[derive(State)]` storage tiers, fluent `el(...)` builder, binary wire
  protocol, no JS/CSS source, no build step. Example reference apps: `../llama-modnitor-rwire`.

## Checks (all must pass before a subtask is done)

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Code rules (inherited from rust-claw)

Zero clippy warnings, **no `#[allow]`/`#[expect]`/`unsafe`**. Names over comments. TDD,
high coverage, regression test per bug. Newtypes for ids, enums over strings, exhaustive
`match` on domain enums. `thiserror` in modules, `anyhow` only at binary edges. Small files
(~300 line / ~40 line soft caps) — split before adding. SQLite is sync `rusqlite` behind
blocking helpers; never block the async runtime.

## rwire UI conventions (the migration is DONE — hold the line)

The escape-hatch migration finished 2026-07: **0 `.attr()`**, ~35 `.style()` (every survivor
genuinely dynamic: `calc()`, `vh`, depth padding, CSS vars), zero hand-rolled chips / badges /
dots / composers. **Full guide:** `../rwire/.claude/skills/rwire/SKILL.md`. rwire is a path dep
we own — when a pattern repeats, promote it upstream instead of copying it (see below).

**The theme owns the aesthetic.** `main.rs`:
`Theme::dark().palette(nord()).style(ThemeStyle::flat()).radius(RadiusScale::None)` gives the
whole terminal look — monospace UI font, hairline borders, no shadows, sharp corners. Never
re-assert it per element: no `FontMono` / `Rounded0` / `FontInheritAll` sprinkles (the base
reset covers form-control fonts; themed components follow the radius scale). Circular things
keep `RoundedFull` — it ignores the radius scale.

**Components first.** Before hand-rolling, check `rwire_components` — several exist *because
this app needed them*: `Chip` (filters/tabs/toggles; chain `.data(...)` for the payload),
`Composer` (chat bar, `.compact(true)` for inline), `ChatScroll` (bottom-pinned autoscroll —
never justify-content on a scroll container), `StatusDot` (presence + pulse + label),
`Badge` (`.shape(Square)` + `.fill(Outline)` for status chips), `Avatar::name(…)` (derives
initials), `EmptyState`, `Drawer`, `Select`, `Input`/`Textarea`, `Button` (incl. `outline`).
App-local shapes live in `ui/widgets.rs` (`md_editor`, `ghost`, `chip`/`accent_submit`
wrappers, `icon_button`, `markdown`) — extend there before inlining a one-off.

**Attributes:** `At`/`Av` tokens or component methods, never string `.attr()`. **Styles:** `St`
tokens; inline `.style()` only for genuinely dynamic values — if a constant lacks a token, add
one in rwire's `style_tokens.rs` (that's how `FieldSizingContent`, `MbAuto`, `HDvh` got there).

**Promote to rwire when a pattern earns it** (repeats ≥2×, or encodes a hard-won fix): add the
component + register it in `libs/rwire-components/src/catalog.rs` (it then appears in the
design-system app automatically), gate rwire (`cargo fmt && clippy -D warnings && test`
workspace-wide), commit there, then adopt here. This app is the framework's first big showcase
— hardening flows both ways.

**Other rules:** wrap changing UI in `#[renderer]` (synced regions); static scaffolding stays plain
`el()`. Lists via `ItemRef` + `iter_with_ref()` + `on_ref()` — no `data-*`/index strings. Navigate
with `Link::to(…)`, never raw `<a href>` (full reload). One routing model only: `on_route` **or**
`routes`+`outlet` — a `Router` without an `outlet()` freezes the page. Colours via semantic `St`
tokens (`St::BgApp`, `St::TextMuted`, …), never hardcoded hex. Width-spanning elements in a
flex-centered parent need `St::WFull` or they collapse to ~0.

## Layout

- `src/db/` — central registry DB (schema + open). Format-compatible with rust-claw's
  central.db (collapsed migration state in `schema.sql`).
- (growing) `src/{state,bridge,app}.rs`, `src/system_map/` — see the plan.
