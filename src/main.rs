//! claw-rwire: a clean-room rewrite of the claw personal-assistant web UI on the
//! rwire framework. The existing `rust-claw` daemon stays the system of record and
//! behavioural reference; this project is written from scratch alongside it.
//!
//! Phase 0: workspace bootstrap — a booting rwire app shell, green tree.

mod db;

use std::error::Error;

use async_std::main;
use rwire::capsule_gen::CapsuleConfig;
use rwire::theme::Theme;
use rwire::{ElementBuilder, Server, St, theme};
use rwire_components::{Container, ContainerSize, Gap, Stack, Text, TextColor};
use rwire_themes::palettes;

const BIND_ADDR: &str = "127.0.0.1:8480";
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

#[main]
async fn main() -> Result<(), Box<dyn Error>> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let _central = db::open(&dir.join("central.db"))?;

    Server::bind(BIND_ADDR)?
        .root(app)
        .capsule_config(CapsuleConfig::new())
        .theme(app_theme())
        .run()
        .await
}

fn app() -> ElementBuilder {
    Container::new()
        .size(ContainerSize::Full)
        .padding(true)
        .child(
            Stack::centered()
                .child(
                    Stack::column()
                        .gap(Gap::Md)
                        .align_center()
                        .children([
                            Text::heading1("claw").color(TextColor::Accent).build(),
                            Text::body("rewrite on rwire — phase 0").build(),
                        ])
                        .build(),
                )
                .build(),
        )
        .build()
        .st([St::BgApp, St::MinHScreen])
}
