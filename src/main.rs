//! claw-rwire: a clean-room rewrite of the claw personal-assistant web UI on the
//! rwire framework. The existing `rust-claw` daemon stays the system of record and
//! behavioural reference; this project is written from scratch alongside it.

mod agent;
mod auth;
mod bridge;
mod ci;
mod db;
mod diff;
mod env_file;
mod gitops;
mod id;
mod kinds;
mod mcp;
mod sandbox;
mod schedule;
mod state;
mod text;
mod tools;
mod ui;
mod workspace;

use std::error::Error;

use async_std::main;
use rwire::capsule_gen::CapsuleConfig;
use rwire::theme::{RadiusScale, Theme};
use rwire::{ProxyResolver, Server, ServerConfig, theme};
use rwire_themes::palettes;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
const BIND_ADDR_ENV: &str = "CLAW_BIND";
const DATA_DIR_ENV: &str = "CLAW_DATA_DIR";

#[theme]
fn app_theme() -> Theme {
    // rust-claw look, now a framework preset: Nord palette + `ThemeStyle::flat()` (hairline
    // borders, no shadows, monospace UI font) + sharp corners. The per-element `FontMono` /
    // `Rounded0` sweeps this app used to carry are gone — the theme owns the aesthetic.
    // (Circular avatars keep `RoundedFull`; `RoundedFull` ignores the radius scale.)
    Theme::dark()
        .palette(palettes::nord())
        .style(rwire::theme::ThemeStyle::flat())
        .radius(RadiusScale::None)
}

/// Installable-PWA config: name/colors matching the Nord dark theme, with branded `>_` icons (the
/// claw masthead glyph). Serves `/manifest.webmanifest` + `/sw.js` + `/pwa/icon-*.png`.
fn pwa_config() -> rwire::pwa::Pwa {
    rwire::pwa::Pwa::new("claw")
        .short_name("claw")
        .description("Your AI assistant — a small team across workspaces.")
        .display(rwire::pwa::PwaDisplay::Standalone)
        .theme_color("#2e3440")
        .background_color("#2e3440")
        .icon(192, include_bytes!("../assets/pwa/icon-192.png").as_slice())
        .icon(512, include_bytes!("../assets/pwa/icon-512.png").as_slice())
        .maskable_icon(
            512,
            include_bytes!("../assets/pwa/icon-512-maskable.png").as_slice(),
        )
}

fn data_dir() -> std::path::PathBuf {
    std::env::var_os(DATA_DIR_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("./claw-data"))
}

fn bind_addr() -> String {
    std::env::var(BIND_ADDR_ENV).unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_owned())
}

/// The reverse-proxy resolver claw hands to rwire: a `/preview/<key>/…` request resolves to the
/// live upstream port for `<key>` (from the preview supervisor), stripping `/preview/<key>` so the
/// branch app sees a root-relative path. Anything else falls through to claw's own handling.
fn preview_proxy() -> ProxyResolver {
    ProxyResolver::new(|path| {
        let key = path.strip_prefix("/preview/")?.split('/').next()?;
        if key.is_empty() {
            return None;
        }
        bridge::preview::port_for(key).map(|port| (port, format!("/preview/{key}")))
    })
}

#[main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_file::load();
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let central_db = dir.join("central.db");
    db::open(&central_db)?;

    let creds = auth::resolve()?;
    if creds.generated {
        println!(
            "Auth: no CLAW_AUTH_TOKEN set — generated a password for user '{}': {}",
            creds.user, creds.password
        );
    } else {
        println!("Auth: HTTP Basic enabled for user '{}'", creds.user);
    }

    let mut server = Server::bind(&bind_addr())?
        .root(ui::app)
        .routes(ui::routes())
        .on_route(ui::on_route())
        .auth(&creds.user, &creds.password)
        .auth_brand("claw")
        .capsule_config(CapsuleConfig::new().pwa(pwa_config()))
        // Same-origin preview proxy (P2): `/preview/<key>/…` forwards to the branch's pooled port,
        // behind claw's own auth gate — agent-built servers never face the internet raw.
        .config(ServerConfig::new().proxy(preview_proxy()))
        .theme(app_theme());

    // Dev convenience: a stable session token (CLAW_DEV_SESSION) survives server restarts, so a
    // `cargo watch` rebuild keeps an open tab logged in and on its current route instead of
    // bouncing it to the login form. Leave unset in production (sessions stay ephemeral).
    if let Some(token) = std::env::var("CLAW_DEV_SESSION")
        .ok()
        .filter(|token| !token.trim().is_empty())
    {
        println!("Auth: dev session enabled — login persists across restarts (CLAW_DEV_SESSION)");
        server = server.dev_session(token);
    }

    bridge::spawn(server.shared_state(), central_db);

    server.run().await
}
