# Bug report: rwire `Drawer` component doesn't reflect `.open(true)`

**Component:** `rwire_components::Drawer` (`libs/rwire-components/src/drawer.rs`)
**Severity:** high — the drawer never visually opens
**Status:** observed + isolated; needs a clean real-interaction repro to 100% confirm (see Caveat)
**Found while:** building the in-situ pipeline step editor in claw-rwire (the New-task drawer is affected too).

## Symptom

A `Drawer` whose bound `open` flag is flipped `false → true` (by a `#[handler]` mutating the state
field) **stays visually closed**. The panel keeps its closed transform.

DOM evidence (right-position drawer, after the open handler fired):

```
panel role=dialog  class=[… u696 …]
  .u696  →  transform: translateX(100%)      ← St::TranslateXFull  (the CLOSED state)
computed transform = matrix(1,0,0,1,320,0)   ← translateX(320px) for a 320px panel
```

`Drawer::build()` pushes `St::TransformNone` when `open`, else `St::TranslateXFull`. The panel has
**`TranslateXFull`**, i.e. it was built with `open == false` — even though the bound field was set
`true` by the handler immediately before.

## Isolation (what rules out the obvious culprits)

- **Not a multi-instance / keyless-sibling morph issue.** Reduced to a **single** `Drawer` on the
  page (`document.querySelectorAll('[role=dialog]').length === 1`) — still fails.
- **The handler runs and the state is mutated.** The server logs the click event for the open
  handler (`open_new_task` → `app.new_task_panel_open = true`).
- **Re-render works in the same renderer.** A hand-built overlay in the *same* `#[renderer]`,
  toggled by an analogous `Option<String>` field, opens and closes correctly. So `#[renderer]`
  re-render + field tracking are fine for sibling state.
- **Class-attribute morphing works.** Filter chips in the same render swap their active class
  (`BgAccent`↔`BgApp`) correctly on re-render.
- **The transform tokens are intact** (checked against the *uncommitted* `style_tokens.rs`):
  `TransformNone = 0x1C0 => "transform:none"`, `TranslateXFull = 0x2B8 => "transform:translateX(100%)"`
  — unchanged. So it is **not** a CSS/token regression.
- Forcing an unrelated re-render of the same renderer (clicking a filter after the open handler)
  does **not** open the drawer either.

## Conclusion

The `Drawer`'s panel is rendered in its **closed** branch even though the bound `open` state is
`true`. Combined with the isolation above (re-render + class morph both work for hand-rolled
elements in the same renderer), the failure is **specific to the `Drawer` component** — its
`self.open` is read as `false` at build time, or the component's output isn't reconciled when its
`open` changes. Custom overlays that toggle by *structure* (render vs `St::DisplayNone`) are
unaffected, which is why the claw step-editor + Peek modal work.

## Regression window

The `Drawer` opened normally **earlier in the same session** (the New-task drawer was seen open).
It broke during in-progress edits to rwire: `libs/rwire-components/src/{button,input}.rs`,
`libs/rwire/src/{attr_tokens,style_tokens}.rs` (rust-analyzer also reports `St::FontInheritAll`,
`At::Autocomplete/Spellcheck/Min/Max/Step`, `Av::Range` as not-found across rwire components — a
token edit in flight). The `style_tokens.rs` diff only **adds** tokens (`FontInheritAll`, `HDvh*`),
so the trigger is more likely in `button.rs` / `input.rs` / `attr_tokens.rs` or the morph, not the
transform tokens.

## Minimal repro (to confirm)

1. Render a single `Drawer::new().position(Right).open(state.flag).on_close(close()).content(…)`.
2. A button whose handler sets `state.flag = true`.
3. Click it. Expect: panel slides in (`transform: none`). Actual: panel stays `translateX(100%)`,
   class `TranslateXFull` — built with `open == false`.

## Suggested checks

- Does `Drawer::build()` still read `self.open` correctly after the component edits? (Diff
  `button.rs`/`input.rs` for shared helpers the Drawer reuses.)
- Does the DOM **morph** update an element's transform/visibility class when only the class set
  changes on a structurally-stable element, *for component-produced subtrees* specifically?
- Sanity-check the `Modal` component the same way (it uses the same backdrop+toggle shape).

## Caveat (confidence)

The repro here was driven mostly via synthetic `dispatchEvent` clicks + DOM inspection. The
hand-built overlay control (same event path) opens/closes correctly, so synthetic events are not
the cause — but a pristine real-mouse repro on a clean page is worth one confirmation pass before
treating this as fully reproduced.

## Workaround (used in claw-rwire)

The step editor and Peek modal are built as custom overlays (backdrop + panel siblings, toggled by
state via render/`DisplayNone`), which don't depend on the `Drawer`'s transform-class toggle. Once
the component is fixed, these can migrate back to `Drawer`/`Modal`.
