use iced::{Element, widget::Space};

use super::mount::Cx;
use crate::{
    atoms::{
        bar::context::{Context, Viewed},
        table::{TableRowData, column_layouts},
    },
    draw::Rect,
    expand::Binding,
    ids::InternId,
    module::TableColumn,
    render::{
        InputOwner, ReadValue, Skin, Tree, UiEvent, Widget, controls::Paint, document::Ctx,
        scope_picker, vis,
    },
    shaping::TextContext,
};

/// The context strip, with the menu its scope face opens.
///
/// The strip is one painted control on both hosts; only the menu is raised by
/// the toolkit, and it hangs off the face the painter reports rather than off
/// the strip.
pub(super) fn context_bar<'a>(
    cx: &Cx<'a, '_, '_>,
    scope: (&[InternId], Option<&Binding>),
    painter: Context,
    data: Viewed,
) -> Element<'a, UiEvent> {
    let (scope_items, scope) = scope;
    let skin = cx.skin;
    if scope_items.is_empty() {
        return Paint::pooled(painter, data, skin, cx.ctx.ui.draw_pools())
            .posed(cx.transform)
            .view();
    }
    let scope_value = scope.and_then(|binding| cx.ctx.read(binding));
    let mut text = TextContext::from(skin.text_resources());
    let Some(face) = painter.face(&mut text, &data) else {
        return Paint::pooled(painter, data, skin, cx.ctx.ui.draw_pools())
            .posed(cx.transform)
            .view();
    };
    scope_picker(
        cx.path,
        scope_items
            .iter()
            .map(|id| cx.ctx.ui.resolve(*id))
            .collect(),
        scope_value.as_ref(),
        skin,
        cx.owner,
        Paint::pooled(painter, data, skin, cx.ctx.ui.draw_pools())
            .posed(cx.transform)
            .view(),
        move |bounds: Rect| Context::placed(face, bounds),
    )
}

pub(super) fn vis<'a>(value: Option<&ReadValue<'_>>, ctx: Ctx<'_, '_>) -> Element<'a, UiEvent> {
    vis::view(value, ctx)
}

pub(super) fn table<'a>(
    cx: &Cx<'a, '_, '_>,
    columns: (&[TableColumn], Option<&Binding>),
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Table(rows)) = cx.value else {
        return Space::new().into();
    };
    let (columns, columns_state) = columns;
    let columns_scope = cx.ctx.scope(columns_state);
    let columns_state = columns_state.map(|binding| cx.ctx.ui.resolve(binding.id));
    let state = columns_state.map(|prefix| (prefix, columns_scope));
    let columns = column_layouts(columns, &cx.ctx, state, cx.skin);
    let rows = rows.iter().map(TableRowData::from).collect();
    crate::render::table(cx.path, rows, columns, cx.skin, cx.owner)
}

pub(super) fn tree<'a>(
    path: &'a str,
    query: Option<&Binding>,
    value: Option<&ReadValue<'_>>,
    ctx: Ctx<'_, '_>,
    skin: &'a Skin,
    owner: InputOwner,
) -> Element<'a, UiEvent> {
    let query = query
        .and_then(|binding| ctx.read(binding))
        .and_then(|value| match value {
            ReadValue::Text(query) => Some(query),
            _ => None,
        })
        .unwrap_or_default();
    Tree::builder()
        .path(path)
        .query(query)
        .maybe_value(value)
        .owner(owner)
        .skin(skin)
        .build()
        .view()
}
