use iced::Element;

use super::mount::Cx;
use crate::{
    atoms::{
        bar::context::{Context, Viewed},
        table::{TableRowData, column_layouts},
    },
    draw::Rect,
    expand::Binding,
    iced::{controls::Paint, immediate::Tree, picker::scope_picker, tree::Widget, vis},
    ids::InternId,
    mount,
    render::{InputOwner, Published, ReadValue, Skin, document::Ctx},
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
) -> Element<'a, Published> {
    let (scope_items, scope) = scope;
    let skin = cx.skin;
    if scope_items.is_empty() {
        return Paint::pooled(painter, data, skin, &cx.ctx.ui.draw_buffers)
            .posed(cx.transform)
            .view();
    }
    let scope_value = scope.and_then(|binding| cx.ctx.read(binding));
    let mut text = TextContext::from(skin.text_resources.as_ref());
    let Some(face) = painter.face(&mut text, &data) else {
        return Paint::pooled(painter, data, skin, &cx.ctx.ui.draw_buffers)
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
        Paint::pooled(painter, data, skin, &cx.ctx.ui.draw_buffers)
            .posed(cx.transform)
            .view(),
        move |bounds: Rect| Context::placed(face, bounds),
    )
}

pub(super) fn vis<'a>(value: Option<&ReadValue<'_>>, ctx: Ctx<'_, '_>) -> Element<'a, Published> {
    vis::view(value, ctx)
}

pub(super) fn table<'a>(
    cx: &Cx<'a, '_, '_>,
    table: &mount::panel::table::host::Table<'_>,
) -> Element<'a, Published> {
    let rows = match cx.value {
        Some(ReadValue::Table(rows)) => *rows,
        _ => &[],
    };
    let state = table
        .columns_state
        .map(|binding| (cx.ctx.ui.resolve(binding.id), cx.ctx.scope(Some(binding))));
    let columns = column_layouts(
        (table.columns, cx.ctx.endpoint(table.width)),
        &cx.ctx,
        state,
        &cx.skin.table,
    );
    let rows = rows.iter().map(TableRowData::from).collect();
    let face = crate::atoms::table::face::TableFace::new(rows, columns, cx.skin, table.frame)
        .with_status(table.status.and_then(|binding| cx.ctx.read(binding)));
    crate::iced::table::table(cx.path, face, cx.owner)
}

pub(super) fn tree<'a>(
    path: &'a str,
    tree: &mount::panel::tree::host::Tree<'_>,
    value: Option<&ReadValue<'_>>,
    ctx: Ctx<'_, '_>,
    skin: &'a Skin,
    owner: InputOwner,
) -> Element<'a, Published> {
    let query = tree.search.then(|| {
        tree.query
            .and_then(|binding| ctx.read(binding))
            .and_then(|value| match value {
                ReadValue::Text(query) => Some(query),
                _ => None,
            })
            .unwrap_or_default()
    });
    Tree::builder()
        .path(path)
        .maybe_query(query)
        .toggle(tree.toggle)
        .maybe_value(value)
        .owner(owner)
        .skin(skin)
        .build()
        .view()
}
