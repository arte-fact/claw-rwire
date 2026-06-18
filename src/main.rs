//! claw-rwire: a clean-room rewrite of the claw personal-assistant web UI on the
//! rwire framework. The existing `rust-claw` daemon stays the system of record and
//! behavioural reference; this project is written from scratch alongside it.

mod auth;
mod bridge;
mod db;
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
    db::open(&dir.join("central.db"))?;

    match auth::init()? {
        auth::TokenSource::Configured => println!("Auth: using CLAW_AUTH_TOKEN"),
        auth::TokenSource::Generated(token) => {
            println!("Auth: no CLAW_AUTH_TOKEN set — generated one for this run: {token}");
        }
    }

    let mut server = Server::bind(&bind_addr())?
        .root(ui::app)
        .capsule_config(CapsuleConfig::new())
        .theme(app_theme());

    bridge::spawn(server.shared_state());

    server.run().await
}
