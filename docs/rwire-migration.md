# rwire escape-hatch migration

Tracking the migration of `src/ui/` away from rwire escape hatches (`.style()`, `.attr()`,
raw `el(El::Input/Textarea/Option/A)`) toward typed `St`/`At`/`Av` tokens and the
`rwire_components` library. See `CLAUDE.md` → "rwire UI conventions" for the mapping tables.

**Checks per file (all must pass):** `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test`

## Categories

1. **Raw form controls → components.** `el(El::Input/Textarea/Option)` → `Input::…` /
   `Textarea::…` / `Select`+`SelectOption`. Collapses categories 2 & most of 4 at once.
2. **`.set("font","inherit")` → drop it.** Components apply it automatically; standalone raw
   controls get `St::FontInheritAll`.
3. **Constant `.style()` → `St` tokens.** `MinH0`, `OverflowAuto`, `OverflowXAuto`, `MinW0`,
   `DisplayNone`, `Flex1`, `PositionSticky`+`Top0`, `HDvh`.
4. **Raw `.attr()` → `At`/`Av` tokens / component methods.** `type`, `aria-label`,
   `autocomplete`, `required`, `min`, `rows`, `spellcheck`, etc.
5. **Raw `el(El::A).attr("href")` → `Link::to`.**

### Known component limits (keep the escape hatch, note it)
- `Button` has no submit-type method → `.at(At::Type, Av::Submit)` on the built button is OK.
- Genuinely dynamic `.style()` stays: depth padding (`files.rs`), custom flex-basis / `vh`
  (`chat.rs`), `fill()` (`mod.rs`).

## Phasing

Component conversion (#1) imposes the component design system (`Input` →
`RoundedMd`/fixed height/`BorderPrimary` focus; `Select` wraps the `<select>` in a div +
arrow) — a real **visual + DOM change** that can't be confirmed without a running host. So
the work is split:

- **Phase A — typed styles + typed attributes (#2 #3 #4 #5).** Pure 1:1 token swaps, *zero*
  visual change, fully gated by `fmt`/`clippy`/`test`. ✅ **DONE** for the 5 first-wave files.
- **Phase B — component swaps (#1).** Replace raw `el(El::Input/Textarea/Select)` +
  `input_st()` with `Input`/`Textarea`/`Select`. Changes appearance → **needs host visual
  sign-off**. Not started.

## Progress — Phase A (typed styles/attributes)

| File | #2 font | #3 style→St | #4 attr→At | #5 Link | Phase A |
|---|---|---|---|---|---|
| `workspace_tasks.rs` | ✅ | ✅ | ✅ | — | ✅ done |
| `files.rs` | ✅ | ✅ | ✅ | — | ✅ done |
| `chat.rs` | ✅ | ✅ | ✅ | ✅ | ✅ done |
| `widgets.rs` | ✅ | — | ✅ | — | ✅ done |
| `mod.rs` | — | ✅ | ✅ | ✅ (href kept raw, see note) | ✅ done |

All five: `cargo fmt --check` ✅ · `cargo clippy --all-targets -D warnings` ✅ · `cargo test` (163) ✅.

## Progress — Phase B (components) — TODO

| File | raw controls to convert |
|---|---|
| `workspace_tasks.rs` | `Input`×7 (title/spec/schedule/max_retries/blockers/hidden), `Select`×2, `Option` lists |
| `files.rs` | `Input`×2 (newname/rename), `Textarea`×1 (editor content) |
| `chat.rs` | `Textarea`×1 (composer message) |

(Remaining files — `workspace.rs`, `workspace_workflow.rs`, `prompts.rs`, `review.rs`,
`task_detail.rs`, `dashboard.rs`, `system_map/*`, `activity.rs`, `logs.rs`, `archived.rs`,
`run_chain.rs` — audited but not in the first wave; add rows as tackled.)

## Notes / decisions
- **`mod.rs` logout** stays a raw `el(El::A)` (NOT `Link::route`): `/logout` is a server
  auth route needing a full-page nav to clear the cookie; client routing would 404. Only the
  attr was typed (`.at_str(At::Href, …)`). Documented inline at `logout_link`.
- **`input_st()` helper** now includes `St::FontInheritAll`, so every input/textarea/select
  using it drops its per-field `.style(font:inherit)`. The helper itself is the "local
  rewrite" Phase B will replace with the `Input` component.
- **Genuinely-dynamic `.style()` kept** (correct per the guide): custom flex-basis
  (`0 0 22rem`, `1 1 0`, `0 0 2px`, `0 0 1rem`), depth padding, `12vh`/`40vh`, `field-sizing`,
  `max-width: 960px`/`min(40rem,100%)`, `margin: 0 auto`, drawer `transition`, `fill()`.
- All Phase-A tokens verified present in rwire `style_tokens.rs`/`attr_tokens.rs` before use —
  none needed adding.
