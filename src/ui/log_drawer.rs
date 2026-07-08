//! The shared CI logs drawer (P4c → P5c): one check's full output in a right-side `Drawer`, backed
//! by `App.log_drawer`. Opened from a check badge on the task thread *or* the project page's branch
//! pipeline — both render this one element. `App.log_drawer` is shared state and only one route
//! mounts at a time, so a single shared drawer serves both surfaces.

use rwire::{El, ElementBuilder, St, el, handler};
use rwire_components::{Drawer, DrawerPosition};

use crate::bridge::{self, Command};
use crate::state::App;

/// The logs drawer element — open on `App.log_drawer`'s content, else closed.
pub(crate) fn drawer(app: &App) -> ElementBuilder {
    let body = match &app.log_drawer {
        Some(view) => el(El::Div)
            .st([
                St::WFull,
                St::PSm,
                St::BgApp,
                St::WhitespacePreWrap,
                St::BreakWords,
                St::TextXs,
                St::OverflowYAuto,
            ])
            .text(&view.log),
        None => el(El::Div),
    };
    let title = app
        .log_drawer
        .as_ref()
        .map(|view| view.title.clone())
        .unwrap_or_default();
    Drawer::new()
        .position(DrawerPosition::Right)
        .open(app.log_drawer.is_some())
        .title(title)
        .on_close(dismiss())
        .content(body)
        .build()
}

/// Close the logs drawer — clears `App.log_drawer` via the bridge.
#[handler]
fn dismiss(_ctx: &rwire::EventContext) {
    bridge::send(Command::CloseLogDrawer);
}
