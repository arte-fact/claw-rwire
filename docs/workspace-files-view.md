# Workspace Files view — browse + edit + manage (ported from rust-claw)

> Step 2 follow-up. Ports rust-claw's well-liked Files manager onto claw-rwire's
> Command/renderer model. **Editing is included** — this is a full file manager, not a
> read-only browser.

## Context

Step 2 gave agents real workspaces under `<data>/workspaces/<folder>/` but no UI to see or
edit what's there. rust-claw's Files manager (`templates/files.html`, `src/web/files.rs`,
`src/workspace/ops.rs`) is a two-pane browser — directory list + file viewer — with **inline
editing**, plus new-file / new-folder / rename / delete (and upload/download).

rust-claw implements it as REST endpoints + `assets/claw.js`. claw-rwire has no JS/REST
layer, so each endpoint maps to a **`Command`** handled on the bridge thread (which owns fs +
DB) that projects into `App`; the view is rwire renderers. The path-jailing, size caps, and
binary handling port verbatim (framework-agnostic). Reference: rust-claw `src/workspace/ops.rs`
(`list`/`read_text`/`write_text`/`mkdir`/`delete`/`rename`/`mutable_target`) and
`src/web/files.rs` (handlers + the `coder_folder` workspace lookup).

## Approach

All blocking on the bridge thread. The UI sends commands; the bridge lists/reads/writes under
the **jailed** workspace and projects into `App`. Each mutating op re-lists the current dir.

### 1. `src/workspace/ops.rs` (new) — port rust-claw's `ops.rs`
Everything resolves through the existing `jail` (`src/workspace/path.rs`).
- `Entry { name, kind: EntryKind (Dir|File|Symlink), size: u64, modified: Option<u64> }`.
- `list(ws, rel)` — `read_dir` + `symlink_metadata`; sort **dirs first, then case-insensitive
  name**.
- `read_text(ws, rel)` — must be a file, **≤ `FILE_VIEW_MAX_BYTES` (1 MiB)**, reject NUL /
  non-UTF-8 as `Binary`.
- `write_text(ws, rel, content)` — via `mutable_target` (rejects the root); parent must exist.
  Also used to create an empty file.
- `mkdir`, `delete` (file → `remove_file`, dir → `remove_dir_all`),
  `rename(from, to)` (src exists, dst doesn't), and the `mutable_target` guard.
- `WorkspaceError` already expanded in `src/workspace/mod.rs` with `NotFound`,
  `NotADirectory`, `NotAFile`, `Binary`, `TooLarge`, `InvalidName`, `Exists`
  (`FILE_VIEW_MAX_BYTES` constant added too).

### 2. State (`src/state.rs`)
- `Tab::Files` (+ `NAV`, `label` "Files", `path` "/files", `icon` — add `Icon::Folder` to
  rwire icons if absent).
- Read-models: `FileEntry { name, kind, size }`, `OpenFile { path, content, editable, note }`
  (binary / too-large → `editable=false` + a `note`).
- `App` fields: `files_agent: Option<String>`, `files_path: String`,
  `files_entries: Vec<FileEntry>`, `files_open: Option<OpenFile>`,
  `files_new_name: String` (controlled toolbar name), `files_rename: Option<String>`
  (row being renamed), `files_pending_delete: Option<String>`, `files_error: Option<String>`.

### 3. Bridge commands + handlers (`src/bridge/mod.rs`)
Resolve once per command: `agents::get(agent)?.folder` → `workspace::dir_for(data_dir(),
folder)`. Failures surface into `files_error`.
- `OpenFiles` — sent by `on_route` for `/files`; default agent (`agents::web_agent` ?:
  `agents::first`), `list` root.
- `ListDir { agent, path }` — `ops::list`; project agent/path/entries, clear `files_open`.
- `OpenFile { agent, path }` — `ops::read_text` (or a non-editable note); project `files_open`.
- `WriteFile { agent, path, content }` — `ops::write_text` (**save**, and create-empty).
- `NewFolder { agent, path }` → `ops::mkdir`; `DeleteEntry { agent, path }` → `ops::delete`;
  `RenameEntry { agent, from, to }` → `ops::rename`. Each re-lists + projects.

### 4. UI (`src/ui/files.rs` new + `src/ui/mod.rs`)
- `mod files`; `routes()` += `.page("/files", |_| view(files::page()))`; `on_route` sends
  `OpenFiles` for `/files`; `Tab::NAV` auto-lists the nav row.
- `files::page()` — `widgets::page_header` + an **agent `<select>`** (`app.agents`; change →
  `ListDir{agent,""}`) + a two-pane body (stacks on mobile):
  - **Left — browser**: breadcrumb of `files_path` (segment → `ListDir` prefix); toolbar
    (controlled `files_new_name` input + **New file** → `WriteFile{…,""}` and **New folder** →
    `NewFolder`); a `..` row off-root; one row per `FileEntry` — folder (descend) or file
    (`OpenFile`), each with inline **rename** (controlled input swap) and **delete** (inline
    confirm via `files_pending_delete`, mirroring `src/ui/archived.rs`). Entry name rides a
    `data-path`/`data-name` attr.
  - **Right — viewer/editor**: `files_open` → a `<form>` with the filename header, Save
    button, and `<textarea name="content">` seeded with the content, **keyed by path** (resets
    on file switch — same trick as the chat composer). Save reads `ctx.field("content")` →
    `WriteFile`. `editable=false` → show the `note` instead of the editor.
  - `files_error` banner when set.
- Reuse `widgets::page_header`, `archived.rs`'s button + inline-confirm pattern, and
  `ui/system_map/mod.rs`'s `select_st`/`text_input` styling.

## Files
- New: `src/workspace/ops.rs`, `src/ui/files.rs`.
- Edit: `src/workspace/mod.rs` (`pub mod ops` + expanded `WorkspaceError` — done),
  `src/state.rs`, `src/bridge/mod.rs`, `src/ui/mod.rs`, and `rwire/.../icons.rs` (Folder icon
  if missing).

## Verification
- `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test`, with unit
  tests on `ops`: list ordering + jail rejection (`../etc`), `read_text` size/binary caps,
  write/mkdir/rename/delete round-trips, `mutable_target` rejecting the root.
- E2E (Playwright, login-form pattern): on `/files`, open the coder agent's workspace, see
  `AGENT.md` + a written file; click `AGENT.md` → editor loads it; edit + Save → reopen shows
  the change and disk changed; create a file/folder; rename; delete (with confirm); confirm
  `../escape` is rejected. Screenshot the two-pane view.

## Out of scope (this increment)
- **Upload / download** — binary transfer needs dedicated rwire `Server` routes (like the
  `/logout` gate route) + an `<input type=file>` multipart post, outside the Command/render
  model. Follow-up.
- Syntax highlighting (rust-claw uses a plain textarea too); live refresh / file watching.
