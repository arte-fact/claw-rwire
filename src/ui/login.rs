//! The login gate: a centered card with a single token field. Submitting reads the
//! token on send (form field, no per-keystroke traffic) and checks it against the
//! configured secret; a failed attempt flips `Auth::failed` for the inline error.

use rwire::{El, ElementBuilder, Ev, St, el, handler};
use rwire_components::{Button, Card, CardPadding, FormField, Gap, Input, Stack, Text};

use crate::state::Auth;

pub fn login_form(failed: bool) -> ElementBuilder {
    let mut card_children = vec![
        Text::heading2("claw").accent().build().st([St::FontMono]),
        Text::body("Sign in to continue").muted().build(),
        token_form(),
    ];
    if failed {
        card_children.push(Text::body("Invalid token").error().build());
    }

    el(El::Div)
        .st([
            St::PositionFixed,
            St::Inset0,
            St::Z50,
            St::BgApp,
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyCenter,
            St::PLg,
        ])
        .append([Card::new()
            .padding(CardPadding::Lg)
            .child(Stack::column().gap(Gap::Md).children(card_children).build())
            .build()])
}

fn token_form() -> ElementBuilder {
    el(El::Form)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .on(Ev::Submit, submit_login())
        .append([
            FormField::new()
                .label("Access token")
                .input(
                    Input::password()
                        .name("token")
                        .id("token")
                        .placeholder("••••••••")
                        .build(),
                )
                .build(),
            Button::primary("Sign in")
                .full_width(true)
                .build()
                .attr("type", "submit"),
        ])
}

#[handler]
fn submit_login(auth: &mut Auth, ctx: &EventContext) {
    let candidate = ctx.field("token").unwrap_or_default();
    if crate::auth::verify(candidate) {
        auth.authenticated = true;
        auth.failed = false;
    } else {
        auth.failed = true;
    }
}
