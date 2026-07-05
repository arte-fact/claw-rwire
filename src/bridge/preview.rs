//! The preview supervisor (P2): a running server per request branch (and a long-lived develop
//! staging preview per web project), reachable at a pooled local port. In-memory and process-only —
//! previews die with the server, so there is **no table**; the map starts empty on boot and
//! develop-staging is re-launched on the next delivery. Start runs the project's kind preview command
//! (rwire-app: `cargo run`; node-app: `npm start`; or a per-project override) from the lane (or the
//! canonical root, for staging) under its exec strategy, reusing the P1b build artifact.
//!
//! Stopping a preview is deliberate. An isola preview is an `isola exec` host process that *parents*
//! the in-sandbox server across a PID namespace, so reaping that handle does not reap the server (it
//! reparents). Instead every preview marks its environment with a unique `CLAW_PREVIEW_ID`, and stop
//! sweeps `/proc/*/environ` for that marker — inherited by `cargo` *and* the binary it spawns, so the
//! whole tree dies. A host preview is spawned in its own process group and killed by group. At
//! deliver/abandon the lane sandbox is destroyed anyway, which is the backstop reaper.

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::db::settings;
use crate::kinds;
use crate::tools::exec::{self, ExecStrategy};

/// Default local port pool when `CLAW_PREVIEW_PORTS` is unset. Same-origin previews proxy to these.
const DEFAULT_PORTS: (u16, u16) = (7900, 7920);
/// How long to wait for a freshly-spawned preview to answer on its port before reporting it as
/// still starting (a warm `cargo run` is seconds; a cold build can be longer — it keeps building).
const HEALTH_DEADLINE: Duration = Duration::from_secs(90);
const PROBE_INTERVAL: Duration = Duration::from_millis(400);
/// Wall-clock cap for the in-sandbox marker sweep (a quick `/proc` scan).
const SWEEP_TIMEOUT: Duration = Duration::from_secs(15);

/// One live preview. `strategy` and `id` are what `stop` needs to reap the real server process.
struct Preview {
    port: u16,
    child: Child,
    strategy: ExecStrategy,
    id: String,
    log: PathBuf,
}

/// The bridge-owned supervisor map, keyed by preview id (`<ws>-<short>`). Mirrors the module-level
/// `OnceLock<Mutex<HashMap<…>>>` precedent the bridge uses for background state.
static PREVIEWS: OnceLock<Mutex<HashMap<String, Preview>>> = OnceLock::new();

fn table() -> &'static Mutex<HashMap<String, Preview>> {
    PREVIEWS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start (or restart in place) the preview for a task's lane, returning the human status to hand
/// the agent (up-and-at-`<uri>`, or still-building with a log tail). Resolves the target repo's
/// command (per-project `preview` override, else the kind default) and spawns it with
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

    // Reuse the port for a stable key (restart) so the proxy path stays valid; else allocate.
    let existing_port = table()
        .lock()
        .ok()
        .and_then(|t| t.get(&key).map(|p| p.port));
    let port = match existing_port {
        Some(port) => port,
        None => {
            free_port().ok_or_else(|| "no free preview port in CLAW_PREVIEW_PORTS".to_owned())?
        }
    };

    // Kill any prior process for this id before rebinding the port, and reap the old handle.
    marker_kill(strategy, &id);
    if let Ok(mut t) = table().lock()
        && let Some(mut old) = t.remove(&key)
    {
        let _ = old.child.kill();
        let _ = old.child.wait();
    }

    let log = log_path(&key);
    let run = start_command(&id, port, &base, &repo, &command);
    let child = exec::spawn_in(strategy, &run, &log)
        .map_err(|e| format!("preview failed to start: {e}"))?;

    let ready = probe(port, HEALTH_DEADLINE);
    if let Ok(mut t) = table().lock() {
        t.insert(
            key.clone(),
            Preview {
                port,
                child,
                strategy: strategy.clone(),
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
    let Some(mut p) = table().lock().ok().and_then(|mut t| t.remove(key)) else {
        return;
    };
    marker_kill(&p.strategy, &p.id);
    if let ExecStrategy::Host { .. } = p.strategy {
        // Host previews are their own process group (spawn_in sets it); kill the whole tree.
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{}", p.child.id()))
            .status();
    }
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
    let keys: Vec<String> = table()
        .lock()
        .map(|t| {
            t.keys()
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
                    "`{repo}` has no preview command (not an rwire/node app, no `preview` override)"
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
    Err("no previewable project in this lane (no rwire/node app, no `preview` override)".to_owned())
}

/// A repo rel's display label — `.` for the meta repo, else the rel itself.
fn label(repo: &str) -> String {
    if repo.is_empty() {
        ".".to_owned()
    } else {
        repo.to_owned()
    }
}

/// The preview command for one repo: the per-project `preview` override if set, else the kind
/// default. Mirrors `ci::checks_for`'s override-then-recipe shape.
fn command_for(conn: &Connection, workspace: &str, repo: &str, dir: &Path) -> Option<String> {
    if let Some(project) = repo.strip_prefix("projects/") {
        let overridden = settings::get(conn, &override_key(workspace, project))
            .ok()
            .flatten()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        if overridden.is_some() {
            return overridden;
        }
        return kinds::kind_for(conn, workspace, project, dir)
            .preview_command()
            .map(str::to_owned);
    }
    kinds::detect(dir).preview_command().map(str::to_owned)
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
/// it spawned. Runs the same `/proc` sweep whether the lane is host or sandbox (via the strategy);
/// for isola it is skipped when the sandbox is already gone, so it never recreates one at teardown.
fn marker_kill(strategy: &ExecStrategy, id: &str) {
    if let ExecStrategy::Isola { sandbox, .. } = strategy
        && !crate::sandbox::exists(sandbox)
    {
        return;
    }
    let needle = sq(&format!("CLAW_PREVIEW_ID={id}"));
    let sweep = |signal: &str| {
        format!(
            "for e in /proc/[0-9]*/environ; do tr '\\0' '\\n' < \"$e\" 2>/dev/null | \
             grep -qxF {needle} && {{ p=${{e#/proc/}}; kill {signal} \"${{p%/environ}}\" 2>/dev/null; }}; done"
        )
    };
    let script = format!("{}; sleep 0.3; {}; true", sweep("-TERM"), sweep("-KILL"));
    let _ = exec::run_in(strategy, &script, SWEEP_TIMEOUT);
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

/// The staging short for a project: `staging-<bare-name>` (the last path segment, so `projects/site`
/// and `site` agree). Kept `/`-free so it stays a single `/preview/<key>` path segment.
fn staging_short(project: &str) -> String {
    let bare = project
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(project);
    format!("staging-{bare}")
}

/// The branch preview key for a request id (`<ws>-<short8>`), for the view layer, which has the
/// request id but not its lane path. Agrees with [`lane_key`] on the request's own tree.
#[must_use]
pub fn request_key(workspace_id: &str, request_id: &str) -> String {
    key_for(workspace_id, short_of_request(request_id))
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
    table().lock().ok().and_then(|t| t.get(key).map(|p| p.port))
}

/// The reverse-proxy decision for one request path: a `/preview/<key>/…` whose key is running
/// resolves to `(upstream port, "/preview/<key>")` — the prefix rwire strips before forwarding.
/// An unknown key or any non-preview path resolves to `None`, so the request falls through to
/// claw's own handling. This is the closure `main.rs` hands to rwire's `ProxyResolver`.
#[must_use]
pub fn resolve_proxy(path: &str) -> Option<(u16, String)> {
    let key = path.strip_prefix("/preview/")?.split('/').next()?;
    if key.is_empty() {
        return None;
    }
    port_for(key).map(|port| (port, uri_for(key)))
}

/// A free port in the pool: in the `CLAW_PREVIEW_PORTS` range, not held by a live preview, and
/// actually bindable (catches a leaked listener from a prior run). `None` if the pool is exhausted.
fn free_port() -> Option<u16> {
    let (lo, hi) = port_range();
    let held: Vec<u16> = table()
        .lock()
        .map(|t| t.values().map(|p| p.port).collect())
        .unwrap_or_default();
    (lo..=hi).find(|port| !held.contains(port) && bindable(*port))
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
    fn command_for_prefers_override_then_kind() {
        let conn = crate::db::open_in_memory().expect("db");
        let dir = std::env::temp_dir().join(format!("claw-prev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname=\"x\"\n[dependencies]\nrwire={path=\"../rwire\"}\n",
        )
        .unwrap();

        // rwire-app kind → cargo run.
        assert_eq!(
            command_for(&conn, "ws", "projects/x", &dir).as_deref(),
            Some("cargo run")
        );
        // Override wins.
        settings::set(&conn, &override_key("ws", "x"), "just serve").unwrap();
        assert_eq!(
            command_for(&conn, "ws", "projects/x", &dir).as_deref(),
            Some("just serve")
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
        let port = table()
            .lock()
            .unwrap()
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
            Some((port, uri_for(&key))),
            "a live key routes to its port with the prefix to strip"
        );
        assert_eq!(resolve_proxy("/preview/ws-nope/x"), None, "unknown key");
        assert_eq!(resolve_proxy("/w/studio"), None, "non-preview path");

        stop(&key);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err(),
            "port {port} should be free after stop"
        );
        assert!(table().lock().unwrap().get(&key).is_none(), "forgotten");
        let _ = std::fs::remove_dir_all(&lane);
    }
}
