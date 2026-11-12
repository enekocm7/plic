//! The clip list's search box and ordering controls.

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use super::app::PlicApp;
use crate::clip_order::{SortKey, StatusFilter};

/// The list's search box and ordering, kept while browsing.
pub struct ClipFilter {
    pub search: Entity<InputState>,
    pub sort: SortKey,
    pub descending: bool,
    pub status: StatusFilter,
    _subscription: Subscription,
}

impl ClipFilter {
    pub fn new(window: &mut Window, cx: &mut Context<PlicApp>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search clips"));
        let subscription = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            search,
            sort: SortKey::default(),
            descending: false,
            status: StatusFilter::default(),
            _subscription: subscription,
        }
    }

    pub fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }
}
