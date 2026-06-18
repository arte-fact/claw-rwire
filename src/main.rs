//! claw-rwire: a clean-room rewrite of the claw personal-assistant web UI on the
//! rwire framework. The existing `rust-claw` daemon stays the system of record and
//! behavioural reference; this project is written from scratch alongside it.

mod auth;
mod bridge;
mod db;
mod id;
mod state;
mod ui;

use std::error::Error;

use async_std::main;
use rwire::capsule_gen::CapsuleConfig;
use rwire::theme::Theme;
use rwire::{Server, theme};
use rwire_themes::palettes;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
const BIND_ADDR_ENV: &str = "CLAW_BIND";
const DATA_DIR_ENV: &str = "CLAW_DATA_DIR";

#[theme]
fn app_theme() -> Theme {
    Theme::dark().palette(palettes::nord())
}

fn data_dir() -> std::path::PathBuf {
    std::env::var_os(DATA_DIR_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("./claw-data"))
}

fn bind_addr() -> String {
    std::env::var(BIND_ADDR_ENV).unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_owned())
}

#[main]
async fn main() -> Result<(), Box<dyn Error>> {
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
        .basic_auth(&creds.user, &creds.password)
        .capsule_config(CapsuleConfig::new())
        .theme(app_theme());

    bridge::spawn(server.shared_state(), central_db);

    server.run().await
}
