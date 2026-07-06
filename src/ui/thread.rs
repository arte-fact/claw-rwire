//! Shared "thread" widgets — the chat-log vocabulary used by both the main chat
//! (`ui::chat`) and the task Thread tab (`ui::task_detail`): a bottom-pinned autoscroll
//! scroller and an **authored entry**, both rwire components. One vocabulary, so the two
//! surfaces read the same.

use rwire::{El, ElementBuilder, St, el};
use rwire_components::{ChatAuthor, ChatEntry, ChatScroll, ChatTag};

/// The scrolling transcript area — rwire's `ChatScroll` (the column-reverse bottom-pinned
/// autoscroll, promoted upstream from this app after the "threads scrolling is broken"
/// regression taught us the auto-margin-not-justify-content rule).
pub fn scroll(inner: ElementBuilder) -> ElementBuilder {
    ChatScroll::new(inner).build()
}

/// An authored entry, rendered through rwire's `ChatEntry`: an intent rail (accent for the agent
/// voice, muted for yours) + a header (`author` · an optional phase `tag` like "Produce · iter 2" ·
/// `time` · an optional `trailing` status element pushed right — a verdict chip or running dot)
/// over the `body` children. `author_color` picks the voice — `St::TextAccent` is the agent,
/// anything else is you. The shared unit of the chat transcript and the task thread.
pub fn entry(
    author: &str,
    author_color: St,
    tag: Option<&str>,
    time: &str,
    trailing: Option<ElementBuilder>,
    body: Vec<ElementBuilder>,
) -> ElementBuilder {
    let chat_author = if author_color.as_u16() == St::TextAccent.as_u16() {
        ChatAuthor::agent(author.to_owned())
    } else {
        ChatAuthor::user(author.to_owned())
    };
    let body = el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs, St::MinW0])
        .append(body);
    let mut entry = ChatEntry::new(chat_author, body);
    if !time.is_empty() {
        entry = entry.time(time.to_owned());
    }
    if let Some(tag) = tag {
        entry = entry.tag(ChatTag::muted(tag.to_owned()));
    }
    if let Some(trailing) = trailing {
        entry = entry.trailing(trailing);
    }
    entry.build()
}
