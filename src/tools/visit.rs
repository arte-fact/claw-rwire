//! The `visit` tool (P2c): a jailed same-origin GET against this branch's running preview, so a
//! reviewer (or a producer debugging) exercises the live server without a browser — "does it serve,
//! and what does it return?" The preview binds `127.0.0.1:<pool-port>` (reachable from the host since
//! isola shares the netns), so the fetch is a plain host-side GET to that port; the response is
//! capped. It's read-only, so both producers and validators carry it.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use super::{ToolDef, ToolOutcome, Toolbox};

/// How long to wait for the upstream to accept and answer — a live preview responds instantly; a
/// dead port fails fast rather than hanging the turn.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap on the returned response — enough to see the served HTML shell / any server-rendered body,
/// without flooding the turn with a large page.
const CAP: usize = 12_000;

/// Bound to one request lane: the workspace id + the lane root, from which the branch preview key is
/// derived (the same key the `preview` tool starts under).
pub struct VisitToolbox {
    workspace_id: String,
    root: PathBuf,
}

impl VisitToolbox {
    #[must_use]
    pub fn new(workspace_id: String, root: PathBuf) -> Self {
        Self { workspace_id, root }
    }
}

impl Toolbox for VisitToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "visit".to_owned(),
            description: "Fetch a page from this branch's running preview server and return the \
                          response (status, headers, and a capped body). Use it to confirm the \
                          branch actually serves — check the running app after a change, or verify \
                          a route responds — without a browser. Pass `path` to fetch a specific \
                          path (default `/`). Requires a preview to be running (start one with the \
                          `preview` tool)."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The path to GET, e.g. `/` or `/about`. Defaults to `/`."
                    }
                }
            }),
        }]
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        if name != "visit" {
            return ToolOutcome::error(format!("error: no tool named {name}"));
        }
        let key = crate::bridge::preview::lane_key(&self.workspace_id, &self.root);
        let Some(port) = crate::bridge::preview::port_for(&key) else {
            return ToolOutcome::text(
                "No preview is running for this branch. Start one with the `preview` tool, then \
                 `visit` it.",
            );
        };
        let requested = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_owned))
            .unwrap_or_else(|| "/".to_owned());
        let path = match sanitize_path(&requested) {
            Ok(path) => path,
            Err(err) => return ToolOutcome::error(format!("error: {err}")),
        };
        match http_get(port, &path) {
            Ok(response) => {
                ToolOutcome::text(format!("GET {path} (preview :{port})\n\n{response}"))
            }
            Err(err) => ToolOutcome::error(format!("error: visiting {path}: {err}")),
        }
    }
}

/// A request path safe to splice into a GET line: leading-slash, no spaces and no control characters
/// (so a crafted `path` can't inject headers via CRLF or smuggle a second request by splitting on a
/// space). An empty/blank path is the root.
fn sanitize_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    if path.chars().any(|c| c.is_control() || c == ' ') {
        return Err("path may not contain spaces or control characters".to_owned());
    }
    Ok(if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    })
}

/// A minimal `HTTP/1.0` GET to `127.0.0.1:<port>` (the jail — only a pooled preview port is ever
/// passed here), reading the response until the server closes or [`CAP`] bytes, whichever first.
fn http_get(port: u16, path: &str) -> Result<String, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    let request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::with_capacity(CAP.min(8192));
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() >= CAP {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    buf.truncate(CAP);
    if buf.is_empty() {
        return Err("preview accepted the connection but returned nothing".to_owned());
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_path_forces_a_leading_slash_and_rejects_crlf() {
        assert_eq!(sanitize_path("about").unwrap(), "/about");
        assert_eq!(sanitize_path("/about").unwrap(), "/about");
        assert_eq!(sanitize_path("").unwrap(), "/");
        // A CRLF-injection attempt is refused, not spliced into the request line.
        assert!(sanitize_path("/x\r\nHost: evil").is_err());
    }

    #[test]
    fn visit_reports_when_no_preview_is_running() {
        let toolbox = VisitToolbox::new("ws".to_owned(), PathBuf::from("/data/trees/ws/deadbeef"));
        let outcome = toolbox.call("visit", "{}");
        assert!(
            outcome.ok,
            "a missing preview is guidance, not a hard error"
        );
        assert!(outcome.result.contains("No preview is running"));
    }

    #[test]
    fn visit_round_trips_a_real_server() {
        use std::io::Write as _;
        use std::net::TcpListener;

        // A one-shot HTTP server on a real port, then point a preview key at it in the map.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut throwaway = [0u8; 512];
                let _ = sock.read(&mut throwaway);
                let _ = sock.write_all(
                    b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<h1>branch is live</h1>",
                );
            }
        });

        let response = http_get(port, "/").expect("gets a response");
        assert!(response.contains("200 OK"));
        assert!(response.contains("branch is live"));
        server.join().ok();
    }
}
