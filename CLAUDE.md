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

## Layout

- `src/db/` — central registry DB (schema + open). Format-compatible with rust-claw's
  central.db (collapsed migration state in `schema.sql`).
- (growing) `src/{state,bridge,app}.rs`, `src/system_map/` — see the plan.
