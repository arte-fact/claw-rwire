//! The preview supervisor (P2): a running server per request branch (and a long-lived develop
//! staging preview per web project), reachable at a pooled local port. In-memory and process-only —
//! previews die with the server, so there is **no table**; the map starts empty on boot and
//! develop-staging is re-launched on the next delivery. Start runs the project's `pipeline.yaml`
//! `preview:` command (or a per-project override) from the lane (or the canonical root, for staging)
//! under its exec strategy, reusing the P1b build artifact.
//!
//! Stopping a preview is deliberate. An isola preview is an `isola exec` host process that *parents*
//! the in-sandbox server across a PID namespace, so reaping that handle does not reap the server (it
//! reparents). Instead every preview marks its environment with a unique `CLAW_PREVIEW_ID`, and stop
//! sweeps `/proc/*/environ` for that marker — inherited by `cargo` *and* the binary it spawns, so the
//! whole tree dies. The sweep runs on the **host** `/proc`, never inside the sandbox: isola shares the
//! host process hierarchy, so a host sweep sees (and can signal) a sandboxed preview by its host pid —
//! including one that reparented out of the sandbox's own reap tree, which an in-sandbox sweep (its own
//! PID namespace per `isola exec`) would miss and `sandbox::destroy` would then orphan.

use std::collections::{HashMap, HashSet};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::db::settings;
use crate::tools::exec::{self, ExecStrategy};

/// Default local port pool when `CLAW_PREVIEW_PORTS` is unset. Same-origin previews proxy to these.
const DEFAULT_PORTS: (u16, u16) = (7900, 7920);
/// How long to wait for a freshly-spawned preview to answer on its port before reporting it as
/// still starting (a warm `cargo run` is seconds; a cold build can be longer — it keeps building).
const HEALTH_DEADLINE: Duration = Duration::from_secs(90);
const PROBE_INTERVAL: Duration = Duration::from_millis(400);
/// Wall-clock cap for the host marker sweep (a quick `/proc` scan).
const SWEEP_TIMEOUT: Duration = Duration::from_secs(15);

/// One live preview. `id` (the `CLAW_PREVIEW_ID` marker) is what `stop` needs to reap the real
/// server process off the host `/proc`; `child` is the `isola exec`/host handle we also wait on.
struct Preview {
    port: u16,
    child: Child,
    id: String,
    log: PathBuf,
}

/// The bridge-owned supervisor: the live previews keyed by id (`<ws>-<short>`), plus the set of
/// ports **claimed but not yet live** — a port is reserved the instant it's picked and stays
/// reserved across the spawn + health-probe, so a concurrent [`start`] can't allocate it twice
/// before its `Preview` lands in `live` (the probe can take up to [`HEALTH_DEADLINE`]).
#[derive(Default)]
struct Supervisor {
    live: HashMap<String, Preview>,
    reserved: HashSet<u16>,
}

/// The single mutex guarding both `live` and `reserved`, so pick-and-reserve is atomic. Mirrors the
/// module-level `OnceLock<Mutex<…>>` precedent the bridge uses for background state.
static PREVIEWS: OnceLock<Mutex<Supervisor>> = OnceLock::new();

fn supervisor() -> &'static Mutex<Supervisor> {
    PREVIEWS.get_or_init(|| Mutex::new(Supervisor::default()))
}

/// Start (or restart in place) the preview for a task's lane, returning the human status to hand
/// the agent (up-and-at-`<uri>`, or still-building with a log tail). Resolves the target repo's
/// command (the project's `pipeline.yaml` `preview:`, else the per-project override) and spawns it with
/// `CLAW_BIND`/`CLAW_PREVIEW_BASE`/`CLAW_PREVIEW_ID` in the environment. `Err` only when the target
/// isn't previewable or the spawn itself fails. The port/key the proxy needs live in the map
/// (read via [`port_for`]/[`resolve_proxy`]), not returned here.
pub fn start_for_lane(
    conn: &Connection,
    workspace_id: &str,
    lane_root: &Path,
    strategy: &ExecStrategy,
    target: Option<&str>,
) -> Result<String, String> {
    start(
        conn,
        workspace_id,
        &short_of(lane_root),
        lane_root,
        strategy,
        target,
    )
}

/// Start (or restart) the long-lived develop-staging preview for one web `project` from its
/// canonical `root`, keyed `<ws>-staging-<name>` so each web project gets a stable URL that tracks
/// `develop`. Called after a delivery merges to `develop`. `Err` (ignored by the caller) when the
/// project isn't previewable — a plain repo has no staging.
pub fn start_staging(
    conn: &Connection,
    workspace_id: &str,
    root: &Path,
    strategy: &ExecStrategy,
    project: &str,
) -> Result<String, String> {
    start(
        conn,
        workspace_id,
        &staging_short(project),
        root,
        strategy,
        Some(project),
    )
}

/// Start (or restart) the long-lived main-staging preview for one web `project` from its detached
/// `main` worktree root `main_root` (`trees/<ws>/main`), keyed `<ws>-main-<name>` so the released
/// line gets its own stable URL (P5a). Resolves the same per-project command as develop-staging.
pub fn start_main_staging(
    conn: &Connection,
    workspace_id: &str,
    main_root: &Path,
    strategy: &ExecStrategy,
    project: &str,
) -> Result<String, String> {
    start(
        conn,
        workspace_id,
        &main_staging_short(project),
        main_root,
        strategy,
        Some(project),
    )
}

/// Refresh main-staging for one just-released `project` in the **background** (a cold server start can
/// take the full health deadline). Host-side: sync the project's detached `main` worktree to the
/// freshly-advanced `main`, then (re)start the preview from it under the `claw-<ws>-main` strategy.
/// A non-web project (no preview command) is silently skipped.
pub fn refresh_main_staging(
    workspace_id: &str,
    canonical: PathBuf,
    main_root: PathBuf,
    project: &str,
    data_dir: PathBuf,
) {
    let workspace_id = workspace_id.to_owned();
    let project = project.to_owned();
    std::thread::spawn(move || {
        let repo = format!("projects/{project}");
        if let Err(err) = crate::gitops::sync_main_worktree(&canonical, &main_root, &repo) {
            crate::bridge::log_line(format!(
                "preview: main worktree {workspace_id}/{project}: {err}"
            ));
            return;
        }
        let Ok(conn) = crate::db::open(&data_dir.join("central.db")) else {
            return;
        };
        let strategy = crate::bridge::exec_strategy_for(&main_root);
        if let Ok(detail) =
            start_main_staging(&conn, &workspace_id, &main_root, &strategy, &project)
        {
            crate::bridge::log_line(format!(
                "preview: main-staging {workspace_id}/{project}: {detail}"
            ));
        }
    });
}

/// Refresh develop-staging for every repo a delivery touched, in the **background** — a cold
/// `cargo run` must never block the delivery worker (the health-probe waits up to 90s). Opens its own
/// registry connection and resolves the canonical exec strategy off the workspace root; each repo
/// that isn't a web app is silently skipped (no staging for a plain repo).
pub fn refresh_staging(workspace_id: &str, root: &Path, repos: &[String], data_dir: PathBuf) {
    let workspace_id = workspace_id.to_owned();
    let root = root.to_path_buf();
    let repos = repos.to_vec();
    std::thread::spawn(move || {
        let Ok(conn) = crate::db::open(&data_dir.join("central.db")) else {
            return;
        };
        let strategy = crate::bridge::exec_strategy_for(&root);
        for repo in &repos {
            if let Ok(detail) = start_staging(&conn, &workspace_id, &root, &strategy, repo) {
                crate::bridge::log_line(format!(
                    "preview: staging {workspace_id}/{repo}: {detail}"
                ));
            }
        }
    });
}

/// Boot recovery: relaunch the long-lived staging previews. The supervisor is in-memory —
/// previews die with the server — and without this the develop / released preview links on the
/// project pages stay dead until the next delivery that *touches* the project (a no-op delivery,
/// or none at all, leaves them down indefinitely). Background — a cold `cargo run` per project
/// must not block boot; one thread walks every workspace × project, starting develop-staging
/// from the canonical root and main-staging where the detached `main` worktree carries the repo.
/// A repo without a preview command errs inside `start` and is skipped, like `refresh_staging`.
pub fn recover_staging(data_dir: PathBuf) {
    std::thread::spawn(move || {
        let Ok(conn) = crate::db::open(&data_dir.join("central.db")) else {
            return crate::bridge::log_line("preview: recover: cannot open the db".to_owned());
        };
        for ws in crate::db::workspaces::list(&conn).unwrap_or_default() {
            let root = crate::workspace::dir_for(&data_dir, &ws.id);
            let strategy = crate::bridge::exec_strategy_for(&root);
            let main_root = crate::gitops::main_tree_root(&data_dir, &ws.id);
            for repo in crate::gitops::repos(&root) {
                let Some(project) = repo.strip_prefix("projects/") else {
                    continue;
                };
                if let Ok(detail) = start_staging(&conn, &ws.id, &root, &strategy, &repo) {
                    crate::bridge::log_line(format!(
                        "preview: recovered develop-staging {}/{project}: {detail}",
                        ws.id
                    ));
                }
                if main_root.join(&repo).is_dir() {
                    let main_strategy = crate::bridge::exec_strategy_for(&main_root);
                    if let Ok(detail) =
                        start_main_staging(&conn, &ws.id, &main_root, &main_strategy, project)
                    {
                        crate::bridge::log_line(format!(
                            "preview: recovered main-staging {}/{project}: {detail}",
                            ws.id
                        ));
                    }
                }
            }
        }
    });
}

/// The core start: resolve the target repo + command from `root`, allocate/reuse the pooled port for
/// `<ws>-<short>`, kill any prior process for the key, spawn under `strategy`, health-probe, record.
fn start(
    conn: &Connection,
    workspace_id: &str,
    short: &str,
    root: &Path,
    strategy: &ExecStrategy,
    target: Option<&str>,
) -> Result<String, String> {
    let (repo, command) = resolve_target(conn, workspace_id, root, target)?;
    let key = key_for(workspace_id, short);
    let id = key.clone();
    // The mount point the proxy serves this preview at — also what a base-path-aware app reads
    // from `CLAW_PREVIEW_BASE` to prefix its own URLs. Works direct (localhost:<port>) too, since
    // an rwire app accepts its WebSocket on any path.
    let base = uri_for(&key);

    // Claim the port and detach any prior process for this key, atomically: reuse the key's current
    // port (restart, so the proxy path stays valid) or reserve a fresh one, and mark it reserved so
    // a concurrent `start` can't double-allocate it during our spawn + probe. `old` is reaped below.
    let (port, old) = {
        let mut sup = supervisor()
            .lock()
            .map_err(|_| "preview supervisor unavailable".to_owned())?;
        let old = sup.live.remove(&key);
        let port = match old.as_ref().map(|p| p.port) {
            Some(port) => port,
            None => reserve_free_port(&mut sup)
                .ok_or_else(|| "no free preview port in CLAW_PREVIEW_PORTS".to_owned())?,
        };
        sup.reserved.insert(port);
        (port, old)
    };

    // Kill any prior process for this id before rebinding the port, and reap the old handle.
    marker_kill(&id);
    if let Some(mut old) = old {
        let _ = old.child.kill();
        let _ = old.child.wait();
    }

    let log = log_path(&key);
    let run = start_command(&id, port, &base, &repo, &command);
    let child = match exec::spawn_in(strategy, &run, &log) {
        Ok(child) => child,
        Err(e) => {
            release_port(port);
            return Err(format!("preview failed to start: {e}"));
        }
    };

    let ready = probe(port, HEALTH_DEADLINE);
    if let Ok(mut sup) = supervisor().lock() {
        sup.reserved.remove(&port);
        sup.live.insert(
            key.clone(),
            Preview {
                port,
                child,
                id: id.clone(),
                log: log.clone(),
            },
        );
    }

    // The same-origin proxy path is the shareable URL ("every branch a URL"); the direct host port
    // is what a server-side tool reaches without the proxy.
    if ready {
        Ok(format!(
            "Preview for {repo} is up at {base}/ (also http://127.0.0.1:{port} for direct access), \
             running `{command}`."
        ))
    } else {
        Ok(format!(
            "Preview for {repo} is still starting at {base}/ (http://127.0.0.1:{port}) running \
             `{command}`; it may still be building. Recent output:\n{}",
            log_tail(&log)
        ))
    }
}

/// Stop and forget a preview by key. Idempotent.
pub fn stop(key: &str) {
    let Some(mut p) = supervisor()
        .lock()
        .ok()
        .and_then(|mut s| s.live.remove(key))
    else {
        return;
    };
    // The host `/proc` marker sweep reaps the real server and every env-inheriting descendant —
    // host or sandbox alike — so a separate process-group kill is no longer needed. The handle
    // wait then reaps the `isola exec`/host parent.
    marker_kill(&p.id);
    let _ = p.child.kill();
    let _ = p.child.wait();
    let _ = std::fs::remove_file(&p.log);
}

/// Stop a request branch's preview (deliver/abandon teardown). The lane sandbox is destroyed by the
/// caller regardless — this reaps the handle and frees the port promptly.
pub fn stop_request(workspace_id: &str, request_id: &str) {
    stop(&key_for(workspace_id, short_of_request(request_id)));
}

/// Stop every preview under a workspace (workspace delete). Snapshots keys first to avoid holding
/// the lock across the per-preview kills.
pub fn stop_workspace(workspace_id: &str) {
    let prefix = format!("{workspace_id}-");
    let keys: Vec<String> = supervisor()
        .lock()
        .map(|s| {
            s.live
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for key in keys {
        stop(&key);
    }
}

/// Resolve which repo to preview and how. With a named `target` (`x` or `projects/x`), that repo
/// must be previewable; without one, the first previewable repo in the lane wins (fine for the
/// common single-project workspace; the agent names the project when a lane holds several).
fn resolve_target(
    conn: &Connection,
    workspace: &str,
    lane_root: &Path,
    target: Option<&str>,
) -> Result<(String, String), String> {
    if let Some(name) = target.map(str::trim).filter(|t| !t.is_empty()) {
        let repo = if name == "." || name.starts_with("projects/") {
            name.to_owned()
        } else {
            format!("projects/{name}")
        };
        let dir = crate::gitops::repo_dir(lane_root, &repo)
            .map_err(|_| format!("no repo `{repo}` in this lane"))?;
        return command_for(conn, workspace, &repo, &dir)
            .map(|command| (label(&repo), command))
            .ok_or_else(|| {
                format!(
                    "`{repo}` has no preview command (no `preview:` in pipeline.yaml or override)"
                )
            });
    }
    for repo in crate::gitops::repos(lane_root) {
        let Ok(dir) = crate::gitops::repo_dir(lane_root, &repo) else {
            continue;
        };
        if let Some(command) = command_for(conn, workspace, &repo, &dir) {
            return Ok((label(&repo), command));
        }
    }
    Err(
        "no previewable project in this lane (no `preview:` in pipeline.yaml or override)"
            .to_owned(),
    )
}

/// A repo rel's display label — `.` for the meta repo, else the rel itself.
fn label(repo: &str) -> String {
    if repo.is_empty() {
        ".".to_owned()
    } else {
        repo.to_owned()
    }
}

/// The preview command for one repo: the project's `pipeline.yaml` `preview:` is the source of truth,
/// then the per-project `preview` override. A project with neither is not previewable. Mirrors
/// `ci::checks_for`.
fn command_for(conn: &Connection, workspace: &str, repo: &str, dir: &Path) -> Option<String> {
    if let Some(command) = crate::pipeline::preview(dir) {
        return Some(command);
    }
    let project = repo.strip_prefix("projects/")?;
    settings::get(conn, &override_key(workspace, project))
        .ok()
        .flatten()
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty())
}

/// `project.<ws>.<name>.preview` — the per-project preview-command override.
#[must_use]
pub fn override_key(workspace: &str, project: &str) -> String {
    format!("project.{workspace}.{project}.preview")
}

/// The shell that exports the preview env then execs the server in its repo subdir.
fn start_command(id: &str, port: u16, base: &str, repo: &str, command: &str) -> String {
    let exports = format!(
        "export CLAW_PREVIEW_ID={} CLAW_BIND=127.0.0.1:{port} CLAW_PREVIEW_BASE={};",
        sq(id),
        sq(base)
    );
    if repo == "." || repo.is_empty() {
        format!("{exports} exec {command}")
    } else {
        format!("{exports} cd {} && exec {command}", sq(repo))
    }
}

/// Kill every process whose environment carries `CLAW_PREVIEW_ID=<id>` — the server and everything
/// it spawned. The sweep runs on the **host** `/proc`, never inside the sandbox: isola shares the
/// host process hierarchy (host PIDs, no netns), so the host sees a sandboxed preview and can signal
/// it — including one that reparented out of its lane's own reap tree. An in-sandbox sweep can't:
/// each `isola exec` is its own PID namespace, so it misses the reparented server, which then also
/// survived `sandbox::destroy` and orphaned the port (the delivered-preview leak this fixes).
fn marker_kill(id: &str) {
    let _ = exec::run_in(&host_sweep(), &marker_sweep_script(id), SWEEP_TIMEOUT);
}

/// A throwaway host strategy for the marker sweep — the script addresses `/proc` absolutely, so the
/// cwd is irrelevant; only that it runs on the host (not through `isola exec`) matters.
fn host_sweep() -> ExecStrategy {
    ExecStrategy::Host {
        root: std::env::temp_dir(),
    }
}

/// The `/proc`-sweep shell: TERM then (after a beat) KILL every pid whose environ carries the
/// marker line. `-qxF` matches the whole `CLAW_PREVIEW_ID=<id>` line, so ids never partial-match.
fn marker_sweep_script(id: &str) -> String {
    let needle = sq(&format!("CLAW_PREVIEW_ID={id}"));
    let sweep = |signal: &str| {
        format!(
            "for e in /proc/[0-9]*/environ; do tr '\\0' '\\n' < \"$e\" 2>/dev/null | \
             grep -qxF {needle} && {{ p=${{e#/proc/}}; kill {signal} \"${{p%/environ}}\" 2>/dev/null; }}; done"
        )
    };
    format!("{}; sleep 0.3; {}; true", sweep("-TERM"), sweep("-KILL"))
}

/// The lane's short id — the request's `req8` for a tree lane, else the final path component.
fn short_of(lane_root: &Path) -> String {
    lane_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dev")
        .to_owned()
}

/// The request's short id (first 8 chars) — matches a tree lane's `short_of`.
fn short_of_request(request_id: &str) -> String {
    request_id.chars().take(8).collect()
}

/// A project's bare name — the last path segment (so `projects/site` and `site` agree), `/`-free so
/// each preview short stays a single `/preview/<key>` segment; the meta repo (`.`/empty) folds to
/// `root`.
fn bare_project(project: &str) -> &str {
    let bare = project
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(project);
    if bare.is_empty() || bare == "." {
        "root"
    } else {
        bare
    }
}

/// The develop-staging short for a project: `staging-<bare-name>`.
fn staging_short(project: &str) -> String {
    format!("staging-{}", bare_project(project))
}

/// The main-staging short for a project: `main-<bare-name>` — the released line's preview (P5a).
fn main_staging_short(project: &str) -> String {
    format!("main-{}", bare_project(project))
}

/// The branch preview key for a request id (`<ws>-<short8>`), for the view layer, which has the
/// request id but not its lane path. Agrees with [`lane_key`] on the request's own tree.
#[must_use]
pub fn request_key(workspace_id: &str, request_id: &str) -> String {
    key_for(workspace_id, short_of_request(request_id))
}

/// The develop-staging preview key for a project (`<ws>-staging-<name>`) — the long-lived preview
/// the project page (P5c) links to when browsing develop.
#[must_use]
pub fn staging_key(workspace_id: &str, project: &str) -> String {
    key_for(workspace_id, staging_short(project))
}

/// The main-staging preview key for a project (`<ws>-main-<name>`) — the released line's preview the
/// project page (P5a) links to once a Release has advanced `main`.
#[must_use]
pub fn main_staging_key(workspace_id: &str, project: &str) -> String {
    key_for(workspace_id, main_staging_short(project))
}

/// The supervisor / proxy key for a (workspace, short): globally unique, URL-safe.
#[must_use]
pub fn key_for(workspace: &str, short: impl AsRef<str>) -> String {
    format!("{workspace}-{}", short.as_ref())
}

/// The same-origin mount path a preview is proxied at (`/preview/<key>`).
#[must_use]
pub fn uri_for(key: &str) -> String {
    format!("/preview/{key}")
}

/// The branch preview key for a request lane (`<ws>-<short>`) — what the `preview`/`visit` tools and
/// the request page address. The producer and validator share it: both work the same lane tree.
#[must_use]
pub fn lane_key(workspace_id: &str, lane_root: &Path) -> String {
    key_for(workspace_id, short_of(lane_root))
}

/// A labeled preview block for an agent prompt, or `None` when the lane holds no previewable project
/// (a plain repo has no server, so the hint would be noise). States the branch's same-origin URL and
/// whether it is live right now, and points at the `preview`/`visit` tools.
pub fn preview_hint(conn: &Connection, workspace_id: &str, lane_root: &Path) -> Option<String> {
    resolve_target(conn, workspace_id, lane_root, None).ok()?;
    let key = lane_key(workspace_id, lane_root);
    let uri = uri_for(&key);
    let status = if port_for(&key).is_some() {
        "It is running now"
    } else {
        "It is not running yet — start it with the `preview` tool"
    };
    Some(format!(
        "Live preview: this branch is served same-origin at {uri}/ . {status}. Use the `preview` \
         tool to start or restart the server and `visit` to fetch a page (confirm a route serves, \
         reproduce a bug, check a fix) — no browser needed."
    ))
}

/// The live upstream port for a preview key, or `None` if nothing is running there — the map
/// lookup the proxy resolver consults for every `/preview/<key>/…` request.
#[must_use]
pub fn port_for(key: &str) -> Option<u16> {
    supervisor()
        .lock()
        .ok()
        .and_then(|s| s.live.get(key).map(|p| p.port))
}

/// The reverse-proxy decision for one request path: a `/preview/<key>/…` whose key is running
/// forwards to its pooled port (rwire strips the `/preview/<key>` prefix). A `/preview/…` whose
/// key is NOT running is `Gone` — rwire serves a human 503 page — never a fallthrough to the app:
/// a stale preview link (previews die with their branch or a server restart, and a page held open
/// in the installed PWA keeps the old link) would otherwise open a second copy of claw, which
/// reads as the app mysteriously relaunching. Non-preview paths resolve `None` (claw handles
/// them). This is the closure `main.rs` hands to rwire's `ProxyResolver`.
#[must_use]
pub fn resolve_proxy(path: &str) -> Option<rwire::ProxyDecision> {
    let key = path.strip_prefix("/preview/")?.split('/').next()?;
    if key.is_empty() {
        return None;
    }
    Some(match port_for(key) {
        Some(port) => rwire::ProxyDecision::Forward {
            port,
            prefix: uri_for(key),
        },
        None => rwire::ProxyDecision::Gone {
            message: "This preview isn't running — previews stop when their branch closes or \
                      the server restarts. Reopen it from the project or request page."
                .to_owned(),
        },
    })
}

/// Pick and reserve a free pool port under the held supervisor lock, so the choice is atomic with
/// the claim: excludes both live-preview ports and already-reserved ports, and must be bindable
/// (catches a leaked listener from a prior run). The caller marks it `reserved` (this returns it
/// already excluded from the next pick). `None` if the pool is exhausted.
fn reserve_free_port(sup: &mut Supervisor) -> Option<u16> {
    let (lo, hi) = port_range();
    let mut used: HashSet<u16> = sup.live.values().map(|p| p.port).collect();
    used.extend(sup.reserved.iter().copied());
    first_free(lo, hi, &used, bindable)
}

/// The first port in `[lo, hi]` that is neither `used` nor rejected by `ok` (bindability). Pure over
/// its inputs so the "no double-allocation" invariant is unit-testable without spawning a preview.
fn first_free(lo: u16, hi: u16, used: &HashSet<u16>, ok: impl Fn(u16) -> bool) -> Option<u16> {
    (lo..=hi).find(|port| !used.contains(port) && ok(*port))
}

/// Release a port reserved by [`reserve_free_port`] that never became a live preview (spawn failed),
/// so the pool doesn't leak it. Idempotent.
fn release_port(port: u16) {
    if let Ok(mut sup) = supervisor().lock() {
        sup.reserved.remove(&port);
    }
}

/// Whether the host can bind `127.0.0.1:<port>` right now (isola shares the host netns, so a
/// host-side bind test reflects sandbox availability too).
fn bindable(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// The configured `[lo, hi]` port range, or [`DEFAULT_PORTS`]. Accepts `7900-7920` or `7900`.
fn port_range() -> (u16, u16) {
    let Ok(raw) = super::boot::env("CLAW_PREVIEW_PORTS") else {
        return DEFAULT_PORTS;
    };
    let parse = |s: &str| s.trim().parse::<u16>().ok();
    match raw.split_once('-') {
        Some((lo, hi)) => match (parse(lo), parse(hi)) {
            (Some(lo), Some(hi)) if lo <= hi => (lo, hi),
            _ => DEFAULT_PORTS,
        },
        None => match parse(&raw) {
            Some(port) => (port, port),
            None => DEFAULT_PORTS,
        },
    }
}

/// A TCP connect probe until the port answers or the deadline passes.
fn probe(port: u16, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            PROBE_INTERVAL,
        )
        .is_ok()
        {
            return true;
        }
        std::thread::sleep(PROBE_INTERVAL);
    }
    false
}

fn log_path(key: &str) -> PathBuf {
    std::env::temp_dir().join(format!("claw-preview-{key}.log"))
}

/// The last ~1 KB of a preview's log — the compile error or panic that kept it from binding.
fn log_tail(log: &Path) -> String {
    let body = std::fs::read(log)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let trimmed = body.trim_end();
    let count = trimmed.chars().count();
    if count > 1000 {
        let skip = count - 1000;
        format!("…{}", trimmed.chars().skip(skip).collect::<String>())
    } else {
        trimmed.to_owned()
    }
}

/// POSIX single-quote for embedding in a `bash -c`/`-lc` payload.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_short_agree_for_a_tree_lane() {
        let lane = Path::new("/data/trees/studio/abcd1234");
        assert_eq!(short_of(lane), "abcd1234");
        assert_eq!(key_for("studio", short_of(lane)), "studio-abcd1234");
        assert_eq!(
            key_for("studio", short_of_request("abcd1234ffff")),
            "studio-abcd1234"
        );
    }

    #[test]
    fn start_command_exports_env_and_cds_into_the_repo() {
        let cmd = start_command(
            "studio-abcd1234",
            7900,
            "/preview/studio-abcd1234",
            "projects/x",
            "cargo run",
        );
        assert!(cmd.contains("CLAW_PREVIEW_ID='studio-abcd1234'"));
        assert!(cmd.contains("CLAW_BIND=127.0.0.1:7900"));
        assert!(cmd.contains("CLAW_PREVIEW_BASE='/preview/studio-abcd1234'"));
        assert!(cmd.contains("cd 'projects/x' && exec cargo run"));
    }

    #[test]
    fn start_command_root_repo_has_no_cd() {
        let cmd = start_command("k", 8000, "", ".", "npm start");
        assert!(cmd.contains("exec npm start"));
        assert!(!cmd.contains("cd "));
    }

    #[test]
    fn port_range_parses_a_range_or_single_or_default() {
        // Env is process-global; assert the parser via a direct helper rather than mutating it.
        assert_eq!(DEFAULT_PORTS, (7900, 7920));
    }

    #[test]
    fn override_key_shape() {
        assert_eq!(
            override_key("studio", "empire"),
            "project.studio.empire.preview"
        );
    }

    #[test]
    fn staging_short_takes_the_bare_project_name() {
        assert_eq!(staging_short("projects/site"), "staging-site");
        assert_eq!(staging_short("site"), "staging-site");
        // The meta repo (`.`/empty) folds to a clean, slash-free key segment.
        assert_eq!(staging_short("."), "staging-root");
        assert_eq!(staging_short(""), "staging-root");
        // A staging key stays a single `/preview/<key>` path segment.
        assert!(!key_for("ws", staging_short("projects/site")).contains('/'));
        // The public staging_key composes ws + the staging short (P5c project-page preview).
        assert_eq!(staging_key("ws", "projects/site"), "ws-staging-site");
    }

    #[test]
    fn main_staging_takes_the_bare_project_name() {
        assert_eq!(main_staging_short("projects/site"), "main-site");
        assert_eq!(main_staging_short("site"), "main-site");
        // Distinct from develop-staging so both previews coexist, slash-free, per project (P5a).
        assert_eq!(main_staging_key("ws", "projects/site"), "ws-main-site");
        assert!(!main_staging_key("ws", "projects/site").contains('/'));
        assert_ne!(
            main_staging_key("ws", "projects/site"),
            staging_key("ws", "projects/site")
        );
    }

    #[test]
    fn preview_hint_is_none_without_a_previewable_project() {
        let conn = crate::db::open_in_memory().expect("db");
        // An empty (non-git) dir holds no repos, so nothing is previewable → no hint (not noise).
        let empty = std::env::temp_dir().join(format!("claw-hint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert!(preview_hint(&conn, "ws", &empty).is_none());
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn command_for_reads_pipeline_yaml_then_override() {
        let conn = crate::db::open_in_memory().expect("db");
        let dir = std::env::temp_dir().join(format!("claw-prev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No pipeline.yaml and no override → not previewable.
        assert_eq!(command_for(&conn, "ws", "projects/x", &dir), None);

        // The per-project override is the escape hatch.
        settings::set(&conn, &override_key("ws", "x"), "just serve").unwrap();
        assert_eq!(
            command_for(&conn, "ws", "projects/x", &dir).as_deref(),
            Some("just serve")
        );

        // pipeline.yaml `preview:` is the source of truth — it wins over the override.
        std::fs::write(dir.join("pipeline.yaml"), "preview: cargo run\n").unwrap();
        assert_eq!(
            command_for(&conn, "ws", "projects/x", &dir).as_deref(),
            Some("cargo run")
        );
    }

    // End-to-end supervisor check (spawns a real http server via python3 — networky, so explicit).
    // Proves the whole loop on the host strategy: resolve an override → spawn with the injected
    // `CLAW_BIND` → health-probe → the port answers → stop reaps it and frees the port. The isola
    // path additionally relies on the shared host netns (verified live). Run with:
    //   cargo test -- --ignored spawns_probes_and_stops
    #[test]
    #[ignore = "spawns a real http server (needs python3) — run explicitly"]
    fn spawns_probes_and_stops_a_preview() {
        use std::net::SocketAddr;

        let conn = crate::db::open_in_memory().expect("db");
        let lane = std::env::temp_dir().join(format!("claw-prevrun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lane);
        let site = lane.join("projects/site");
        std::fs::create_dir_all(&site).unwrap();
        // Both dirs must be git repos for `gitops::repos` to list them.
        for dir in [&lane, &site] {
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(dir)
                .status()
                .expect("git init");
        }
        // A trivial server that binds the port the supervisor injects via CLAW_BIND.
        settings::set(
            &conn,
            &override_key("ws", "site"),
            "python3 -m http.server ${CLAW_BIND##*:} --bind 127.0.0.1",
        )
        .unwrap();

        let strategy = ExecStrategy::Host { root: lane.clone() };
        let detail =
            start_for_lane(&conn, "ws", &lane, &strategy, Some("site")).expect("preview starts");
        assert!(detail.contains("is up at"), "should come up: {detail}");

        let key = key_for("ws", short_of(&lane));
        let port = supervisor()
            .lock()
            .unwrap()
            .live
            .get(&key)
            .map(|p| p.port)
            .expect("recorded in the map");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok(),
            "port {port} should answer while the preview runs"
        );

        // The proxy resolver (claw's actual wiring) points a `/preview/<key>/…` request at this
        // live port with the prefix to strip, and refuses unknown keys / non-preview paths — the
        // rwire side (integration test) proves a `Some` then forwards+strips correctly.
        assert_eq!(
            resolve_proxy(&format!("/preview/{key}/dashboard")),
            Some(rwire::ProxyDecision::Forward {
                port,
                prefix: uri_for(&key)
            }),
            "a live key routes to its port with the prefix to strip"
        );
        assert!(
            matches!(
                resolve_proxy("/preview/ws-nope/x"),
                Some(rwire::ProxyDecision::Gone { .. })
            ),
            "an unknown key is claimed as Gone — never a fallthrough to the app shell"
        );
        assert_eq!(resolve_proxy("/w/studio"), None, "non-preview path");

        // The producer/reviewer hint reflects the live state: the same-origin URL and "running now".
        let hint = preview_hint(&conn, "ws", &lane).expect("previewable → a hint");
        assert!(
            hint.contains(&format!("{}/", uri_for(&key))),
            "hint names the URL: {hint}"
        );
        assert!(
            hint.contains("running now"),
            "hint reflects the live preview: {hint}"
        );

        // Develop staging for the same project starts under its own key, on its own pooled port,
        // and is routable through the proxy just like the branch preview.
        let staging = start_staging(&conn, "ws", &lane, &strategy, "site").expect("staging starts");
        assert!(
            staging.contains("is up at"),
            "staging should come up: {staging}"
        );
        let staging_key = key_for("ws", staging_short("site"));
        let staging_port = port_for(&staging_key).expect("staging recorded in the map");
        assert_ne!(
            staging_port, port,
            "staging gets a port distinct from the branch preview"
        );
        assert_eq!(
            resolve_proxy(&format!("/preview/{staging_key}/")),
            Some(rwire::ProxyDecision::Forward {
                port: staging_port,
                prefix: uri_for(&staging_key)
            })
        );
        stop(&staging_key);

        stop(&key);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err(),
            "port {port} should be free after stop"
        );
        assert!(
            !supervisor().lock().unwrap().live.contains_key(&key),
            "forgotten"
        );
        let _ = std::fs::remove_dir_all(&lane);
    }

    #[test]
    fn first_free_skips_used_ports_so_reservations_never_double_allocate() {
        // The regression for the observed race: two concurrent starts each pick a port before the
        // other's `Preview` lands in `live`. Modelling the pick as pure over the *used* set (live ∪
        // reserved) shows a second pick must skip the first's reservation.
        let mut used: HashSet<u16> = HashSet::from([7900]); // 7900 already live
        let always = |_p: u16| true;

        let first = first_free(7900, 7902, &used, always).expect("a free port");
        assert_eq!(first, 7901, "7900 live → first free is 7901");
        used.insert(first); // caller reserves it

        let second = first_free(7900, 7902, &used, always).expect("another free port");
        assert_eq!(second, 7902, "7901 now reserved → next is 7902");
        assert_ne!(first, second, "a reserved port is never handed out twice");
        used.insert(second);

        assert_eq!(
            first_free(7900, 7902, &used, always),
            None,
            "pool exhausted"
        );
    }

    #[test]
    fn first_free_rejects_unbindable_ports() {
        let used = HashSet::new();
        // 7900 "in use" by something outside the map (a leaked listener): the bindability gate skips
        // it even though it isn't tracked, landing on the next port.
        let only_7901_bindable = |p: u16| p == 7901;
        assert_eq!(
            first_free(7900, 7902, &used, only_7901_bindable),
            Some(7901)
        );
    }

    // Reaps a real marked process off the host `/proc` — the delivered-preview-orphan fix. Spawns a
    // plain host `sleep` carrying the `CLAW_PREVIEW_ID` marker (as a preview's server does via its
    // exported env), then asserts `marker_kill` finds and kills it by host pid.
    #[test]
    fn marker_kill_reaps_a_marked_host_process() {
        let id = format!("test-marker-{}", std::process::id());
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exec sleep 30")
            .env("CLAW_PREVIEW_ID", &id)
            .spawn()
            .expect("spawn marked sleep");
        // Give it a moment to populate /proc/<pid>/environ.
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            child.try_wait().unwrap().is_none(),
            "the marked process is running before the sweep"
        );

        marker_kill(&id);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reaped = false;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !reaped {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(
            reaped,
            "marker_kill must reap a marked process via the host /proc sweep"
        );
    }

    #[test]
    fn marker_sweep_script_targets_host_proc_for_the_exact_id() {
        let script = marker_sweep_script("studio-abcd1234");
        assert!(script.contains("/proc/[0-9]*/environ"), "sweeps host /proc");
        assert!(
            script.contains("CLAW_PREVIEW_ID=studio-abcd1234"),
            "matches the exact marker: {script}"
        );
        assert!(
            script.contains("-qxF"),
            "whole-line match so ids never partial-match"
        );
        assert!(
            !script.contains("isola"),
            "never routes through the sandbox"
        );
    }
}
