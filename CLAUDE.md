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

## rwire UI conventions (migrate toward these)

The UI leans on escape hatches today (124 `.style()`, 148 `.attr()`, 0 `.at()`, raw `el(El::Input)`
forms). Migrate toward first-class tokens and components. **Full guide:**
`../rwire/.claude/skills/rwire/SKILL.md` (read it before touching UI). rwire is a path dep, so its
`St`/`At`/`Av` tokens and component methods are all available immediately.

**Styling — `St` tokens, not `.style()`.** Inline `.style(...)` is *only* for genuinely dynamic
values (`calc()`, depth-based padding, custom flex-basis `"1 1 19rem"`, `vh`). Everything constant
has a token — grep `style_tokens.rs` first:

| `.style(...)` today | use instead |
|---|---|
| `.set("font","inherit")` on form controls (×43) | the `Input`/`Textarea`/`Button`/`Select` components (they apply it), or `St::FontInheritAll` on a raw control |
| `.min_height("0").overflow("auto")` | `St::MinH0`, `St::OverflowAuto` |
| `.min_width("0")` · `.display("none")` | `St::MinW0` · `St::DisplayNone` |
| `.set("flex","1")` · `.set("flex-wrap","wrap")` | `St::Flex1` · `St::FlexWrap` |
| `.set("white-space","pre-wrap")` | `St::WhitespacePreWrap` |
| `.set("position","sticky").set("top","0")` | `St::PositionSticky` + `St::Top0` |
| `height:100dvh` / any `100vh` full-height | `St::HDvh` / `St::MinHDvh` (never raw `100vh`) |

**Attributes — components first, then `At`/`Av` tokens, raw `.attr()` last.** Build inputs from the
`rwire_components` form types, not raw `el(El::Input)`:

| raw today | use instead |
|---|---|
| `el(El::Input).attr("type","email").attr("name","x").attr("placeholder",…)` | `Input::email().name("x").placeholder(…)` |
| `.attr("autocomplete","off")` (×16) | `Input::…autocomplete("off")` / `Textarea::…autocomplete("off")` (or `.at(At::Autocomplete, Av::Off)`) |
| `.attr("min"/"max"/"step", …)` | `Input::number().min("0").max("100").step("1")` |
| `.attr("spellcheck",…)` · `.attr("rows",…)` | `…spellcheck(false)` · `Textarea::…rows(n)` |
| any other `.attr("k","v")` | `.at(At::K, Av::V)` / `.at_str(At::K, v)` / `.bool_attr(At::K)`; if no token exists, add one in rwire's `attr_tokens.rs` |

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
