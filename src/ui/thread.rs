//! Shared "thread" widgets — the chat-log vocabulary used by both the main chat
//! (`ui::chat`) and the task Thread tab (`ui::task_detail`): a bottom-pinned autoscroll
//! scroller and an **authored entry** (an author + optional phase tag + time header over a
//! body, with an optional left accent rail). One vocabulary, so the two surfaces read the
//! same.

use rwire::{El, ElementBuilder, St, el};
use rwire_components::ChatScroll;

use super::chat::accent_bar;

/// The scrolling transcript area — rwire's `ChatScroll` (the column-reverse bottom-pinned
/// autoscroll, promoted upstream from this app after the "threads scrolling is broken"
/// regression taught us the auto-margin-not-justify-content rule).
pub fn scroll(inner: ElementBuilder) -> ElementBuilder {
    ChatScroll::new(inner).build()
}

/// An authored entry: a header (`author` in `author_color`, an optional phase `tag` like
/// "Produce · iter 2", the `time`, and an optional `trailing` element pushed to the right — a
/// verdict chip or a running dot) over the `body` children, with an optional left accent rail.
/// The shared unit of the chat transcript and the task thread.
pub fn entry(
    author: &str,
    author_color: St,
    tag: Option<&str>,
    time: &str,
    trailing: Option<ElementBuilder>,
    accent: bool,
    body: Vec<ElementBuilder>,
) -> ElementBuilder {
    let mut header = vec![el(El::Span).st([St::FontBold, author_color]).text(author)];
    if let Some(tag) = tag {
        header.push(el(El::Span).st([St::TextXs, St::TextMuted]).text(tag));
    }
    if !time.is_empty() {
        header.push(el(El::Span).st([St::TextLow, St::TextXs]).text(time));
    }
    if let Some(trailing) = trailing {
        // A spacer pushes the trailing element (verdict chip / running dot) to the right edge.
        header.push(el(El::Span).st([St::Flex1]));
        header.push(trailing);
    }
    // `MinW0` + `BreakWords` let the body wrap at the column edge instead of forcing a
    // horizontal scroll; `Flex1` lets the block fill the row beside an accent rail.
    let block = el(El::Div)
        .st([
            St::Flex1,
            St::MinW0,
            St::BreakWords,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append([
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append(header),
            el(El::Div)
                .st([St::DisplayFlex, St::FlexCol, St::GapXs, St::MinW0])
                .append(body),
        ]);
    let mut row = Vec::new();
    if accent {
        row.push(accent_bar(true));
    }
    row.push(block);
    el(El::Div)
        .st([St::DisplayFlex, St::GapMd, St::MinW0])
        .append(row)
}
