use iced::Element;

use super::{
    icon::render_tree_icon,
    read::{read_scope, resolve},
};
use crate::{
    compile::CompiledUi,
    expand::Binding,
    ids::InternId,
    module::{DeckSummaryStyle, TrackColumn},
    render::{ReadValue, Reads, Skin, UiEvent},
    widgets::{
        Widget,
        deck::{DeckSummary, Time},
        nav::{ContextBar, Tree},
        track_list::TrackList,
        vis::Vis,
    },
};

pub(super) fn deck_summary<'a>(
    style: DeckSummaryStyle,
    value: Option<&ReadValue<'_>>,
    scope: &str,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    DeckSummary::builder()
        .style(style)
        .maybe_value(value)
        .scope(scope)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn time<'a>(
    value: Option<&ReadValue<'_>>,
    scope: &'a str,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Time::builder()
        .maybe_value(value)
        .scope(scope)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn vis<'a>(value: Option<&ReadValue<'_>>, reads: &dyn Reads) -> Element<'a, UiEvent> {
    Vis::builder()
        .maybe_preset(value)
        .reads(reads)
        .build()
        .view()
}

pub(super) fn track_list<'a>(
    path: &'a str,
    columns: &[TrackColumn],
    columns_state: Option<&Binding>,
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    TrackList::builder()
        .path(path)
        .columns(columns)
        .maybe_columns_state(columns_state.map(|binding| ui.resolve(binding.id)))
        .columns_scope(read_scope(columns_state, ui))
        .maybe_value(value)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn browser_tree<'a>(
    path: &'a str,
    query: Option<&Binding>,
    value: Option<&ReadValue<'_>>,
    ui: &CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let query = query
        .and_then(|binding| resolve(reads, binding, ui))
        .and_then(|value| match value {
            ReadValue::Text(query) => Some(query),
            _ => None,
        })
        .unwrap_or_default();
    Tree::builder()
        .path(path)
        .query(query)
        .maybe_value(value)
        .icon(render_tree_icon)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn context_bar<'a>(
    path: &'a str,
    scope_items: &[InternId],
    scope: Option<&Binding>,
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let scope_value = scope.and_then(|binding| resolve(reads, binding, ui));
    ContextBar::builder()
        .path(path)
        .scope_items(scope_items.iter().map(|id| ui.resolve(*id)).collect())
        .maybe_scope_value(scope_value.as_ref())
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}
