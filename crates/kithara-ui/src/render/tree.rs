use iced::{
    Alignment, Background, Color, Element, Length, Padding,
    alignment::{Horizontal, Vertical},
    widget::{Column, Row, Space, Stack, container, container::Style as ContainerStyle},
};
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{
        chip::Chip,
        design::{
            cell::Cell, crossfader::Crossfader, meter::Meter, segmented::Segmented, select::Select,
            status_dot::StatusDot, swatch::Swatch,
        },
        knob::Knob,
        meter::StereoMeter,
        readout::Readout,
        toggle::{Checkbox, Toggle},
        vu::VerticalVu,
    },
    compile::{CompiledNode, CompiledUi},
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::{Axis, FrameSides},
    module::{
        ButtonStyle, ChipStyle, ChromeStyle, DeckSummaryStyle, FaderStyle, GlyphStyle, IconName,
        TextAlign, TextStyle, Tone, TrackColumn, WindowControlsStyle,
    },
    render::{Icon, ReadValue, Reads, Skin, TreeIcon, UiEvent, WindowEdge},
    size::{Dim, SizeSpec, control_size},
    skin::ColorRole,
    widgets::{
        ModuleChrome, Widget,
        button::ControlButton,
        deck::{Bpm, DeckSummary, Time},
        fader::Fader,
        frame_overlay,
        global_bar::{Brand, Divider, PresetSelector, SettingsButton, Spacer},
        mini_wave::MiniWave,
        nav::{ContextBar, Glyph, NavItem, TabLarge, Tree},
        telemetry::Telemetry,
        text::Text,
        track_list::TrackList,
        vis::Vis,
        wave::zoom_math::DEFAULT_ZOOM,
        window::{ResizeEdge, TitleBar, WindowControls, WindowDrag},
    },
};

pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let content = render_compiled(node, ui, reads, skin);
    if ui.resize_edges {
        framed_by_resize_edges(content, skin)
    } else {
        content
    }
}

/// Lays the eight drag zones a system border would have given the window over
/// its edges. They sit above the content rather than beside it, so the layout
/// still gets the whole window; the skin owns their thickness and they paint
/// nothing.
fn framed_by_resize_edges<'a>(content: Element<'a, UiEvent>, skin: &Skin) -> Element<'a, UiEvent> {
    let thickness = Length::Fixed(skin.window.resize_edge);
    let corner = |edge| ResizeEdge::new(edge, thickness, thickness).view();
    let side = |edge, width, height| ResizeEdge::new(edge, width, height).view();
    let edges = Column::with_children([
        Row::with_children([
            corner(WindowEdge::NorthWest),
            side(WindowEdge::North, Length::Fill, thickness),
            corner(WindowEdge::NorthEast),
        ])
        .height(thickness)
        .into(),
        Row::with_children([
            side(WindowEdge::West, thickness, Length::Fill),
            Space::new().width(Length::Fill).height(Length::Fill).into(),
            side(WindowEdge::East, thickness, Length::Fill),
        ])
        .height(Length::Fill)
        .into(),
        Row::with_children([
            corner(WindowEdge::SouthWest),
            side(WindowEdge::South, Length::Fill, thickness),
            corner(WindowEdge::SouthEast),
        ])
        .height(thickness)
        .into(),
    ])
    .width(Length::Fill)
    .height(Length::Fill);
    Stack::with_children([content, edges.into()])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn render_compiled<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    match node {
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => container(
                Row::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(split_length(child_size(child).w, *weight, skin))
                        .height(Length::Fill)
                        .into()
                }))
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Axis::Vertical => container(
                Column::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(Length::Fill)
                        .height(split_length(child_size(child).h, *weight, skin))
                        .into()
                }))
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        },
        CompiledNode::Module {
            module,
            title,
            chip,
            assign,
            chrome,
            frame,
            footer,
            collapsed,
            root,
            ..
        } => {
            let collapsed = *chrome == ChromeStyle::Full
                && matches!(
                    reads.get(ui.resolve(*collapsed)),
                    Some(ReadValue::Bool(true))
                );
            let footer = footer
                .as_ref()
                .and_then(|binding| resolve(reads, binding, ui))
                .and_then(|value| match value {
                    ReadValue::Text(text) => Some(text.to_owned()),
                    _ => None,
                });
            let content: Element<'a, UiEvent> = if collapsed {
                Space::new().into()
            } else {
                render_node(root, ui, reads, skin)
            };
            ModuleChrome::builder()
                .content(content)
                .maybe_title(title.map(|id| ui.resolve(id)))
                .maybe_chip(chip.map(|id| ui.resolve(id)))
                .assign(assign.iter().map(|id| ui.resolve(*id)).collect())
                .style(*chrome)
                .frame(*frame)
                .maybe_footer(footer)
                .on_toggle(UiEvent::ToggleModule(ui.resolve(*module).to_owned()))
                .collapsed(collapsed)
                .skin(skin)
                .build()
                .view()
        }
    }
}

fn render_node<'a>(
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let size = content_size(node, skin);
    let element = match node {
        ExpandedNode::Row {
            children,
            gap,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            ..
        } => bordered(
            filled(
                container(
                    Row::with_children(
                        children
                            .iter()
                            .map(|child| render_node(child, ui, reads, skin)),
                    )
                    .spacing(gap.unwrap_or(skin.layout.grid_gap))
                    .align_y(Alignment::Center)
                    .width(size.0)
                    .height(size.1),
                )
                .padding(padding(*pad, *pad_x, *pad_y, skin))
                .width(size.0)
                .height(size.1),
                *background,
                *background_alpha,
                skin,
            ),
            *frame,
            size,
            skin,
        ),
        ExpandedNode::Column {
            children,
            gap,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            ..
        } => bordered(
            filled(
                container(
                    Column::with_children(
                        children
                            .iter()
                            .map(|child| render_node(child, ui, reads, skin)),
                    )
                    .spacing(gap.unwrap_or(skin.layout.grid_gap))
                    .align_x(Alignment::Center)
                    .width(size.0),
                )
                .padding(padding(*pad, *pad_x, *pad_y, skin))
                .width(size.0)
                .height(size.1),
                *background,
                *background_alpha,
                skin,
            ),
            *frame,
            size,
            skin,
        ),
        ExpandedNode::Slot { children, .. } => container(
            Column::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, skin)),
            )
            .spacing(skin.layout.grid_gap)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .into(),
        ExpandedNode::Control {
            path, spec, read, ..
        } => render_control(*path, spec, read.as_ref(), ui, reads, skin),
    };
    apply_size(element, effective_size(node, skin), node_align(node))
}

/// Container padding: per-axis overrides fall back to `pad`, then to the grid.
fn padding(pad: Option<f32>, pad_x: Option<f32>, pad_y: Option<f32>, skin: &Skin) -> Padding {
    let base = pad.unwrap_or(skin.layout.grid_pad);
    Padding::ZERO
        .top(pad_y.unwrap_or(base))
        .bottom(pad_y.unwrap_or(base))
        .left(pad_x.unwrap_or(base))
        .right(pad_x.unwrap_or(base))
}

/// Paints the container's fill when the node asks for one.
fn filled<'a>(
    element: iced::widget::Container<'a, UiEvent>,
    background: Option<ColorRole>,
    alpha: Option<f32>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(role) = background else {
        return element.into();
    };
    let color = Color {
        a: alpha.unwrap_or(1.0),
        ..skin.color(role)
    };
    element
        .style(move |_| ContainerStyle::default().background(Background::Color(color)))
        .into()
}

/// Wraps a container in hairline borders when the node asks for them.
fn bordered<'a>(
    element: Element<'a, UiEvent>,
    frame: Option<FrameSides>,
    size: (Length, Length),
    skin: &Skin,
) -> Element<'a, UiEvent> {
    match frame {
        Some(sides) => frame_overlay(element, sides, size, skin),
        None => element,
    }
}

/// The lengths a container takes, as the document declared them. A node that
/// measures its content must carry `Shrink` all the way down, or the first
/// `Fill` inside it claims the whole row.
fn content_size(node: &ExpandedNode, skin: &Skin) -> (Length, Length) {
    effective_size(node, skin).map_or((Length::Fill, Length::Fill), |size| {
        (
            length_for(size.w, Length::Fill),
            length_for(size.h, Length::Fill),
        )
    })
}

fn render_control<'a>(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let value = read.and_then(|binding| resolve(reads, binding, ui));
    let value = value.as_ref();
    let path = ui.resolve(path);
    let scope = read_scope(read, ui);
    match spec {
        ControlSpec::DeckSummary { style } => {
            render_deck_summary(*style, value, scope, reads, skin)
        }
        ControlSpec::Brand => Brand::builder().skin(skin).build().view(),
        ControlSpec::Spacer => Spacer::builder().skin(skin).build().view(),
        ControlSpec::Divider => Divider::builder().skin(skin).build().view(),
        ControlSpec::PresetSelector => PresetSelector::builder()
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::SettingsButton => SettingsButton::builder().skin(skin).build().view(),
        ControlSpec::WindowDrag => WindowDrag.view(),
        ControlSpec::TitleBar { label } => render_titlebar(*label, ui, skin),
        ControlSpec::WindowControls { style } => render_window_controls(*style, skin),
        ControlSpec::Bpm { placeholder } => Bpm::builder()
            .maybe_placeholder(placeholder.map(|id| ui.resolve(id)))
            .maybe_value(value)
            .scope(scope)
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Time => render_time(value, scope, reads, skin),
        ControlSpec::Text {
            style,
            label,
            active,
            ..
        } => render_text(
            *style,
            label.map(|id| ui.resolve(id)),
            read_flag(active.as_ref(), reads, ui),
            value,
            skin,
        ),
        ControlSpec::Glyph { icon, style } => render_glyph(*icon, *style, skin),
        ControlSpec::NavItem { label, icon } => {
            render_nav_item(path, ui.resolve(*label), *icon, value, skin)
        }
        ControlSpec::TabLarge { label } => render_tab_large(path, ui.resolve(*label), value, skin),
        ControlSpec::Button {
            label,
            icon,
            active_label,
            style,
        } => render_button(
            path,
            ui.resolve(*label),
            *icon,
            active_label.map(|id| ui.resolve(id)),
            *style,
            value,
            skin,
        ),
        ControlSpec::Scalar { format, framed } => Telemetry::builder()
            .format(*format)
            .framed(*framed)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Crossfader { ticks } => render_crossfader(path, *ticks, value, skin),
        ControlSpec::Fader { style, label } => {
            render_fader(path, *style, label.map(|id| ui.resolve(id)), value, skin)
        }
        ControlSpec::Toggle => Toggle::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Checkbox => Checkbox::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Segmented { items } => render_segmented(path, items, value, ui, skin),
        ControlSpec::Select { label } => render_select(*label, ui, skin),
        ControlSpec::StatusDot { label, tone } => render_status_dot(*label, *tone, ui, skin),
        ControlSpec::Swatch { role, label } => render_swatch(*role, *label, ui, skin),
        ControlSpec::Cell { label, highlighted } => render_cell(*label, *highlighted, ui, skin),
        ControlSpec::Readout {
            label,
            tone,
            framed,
        } => render_readout(*label, *tone, *framed, value, ui, skin),
        ControlSpec::Chip { label, style } => {
            render_chip(path, ui.resolve(*label), *style, value, skin)
        }
        ControlSpec::Knob { label } => {
            render_knob(path, label.map(|id| ui.resolve(id)), value, skin)
        }
        ControlSpec::VuStereo => StereoMeter::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::VuVertical { ticks } => VerticalVu::builder()
            .path(path)
            .ticks(*ticks)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Vis => render_vis(value, reads),
        ControlSpec::Wave { style, badge, zoom } => MiniWave::builder()
            .path(path)
            .style(*style)
            .zoom(wave_zoom(zoom.as_ref(), reads, ui))
            .maybe_badge(badge.map(|id| ui.resolve(id)))
            .maybe_value(value)
            .scope(scope)
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Meter => Meter::builder()
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::TrackList {
            columns,
            columns_state,
            assign,
        } => render_track_list(
            TrackListParts {
                path,
                columns,
                columns_state: columns_state.as_ref(),
                assign,
                value,
            },
            ui,
            reads,
            skin,
        ),
        ControlSpec::Tree { query } => render_tree(path, query.as_ref(), value, ui, reads, skin),
        ControlSpec::ContextBar { scope_items, scope } => {
            render_context_bar(path, scope_items, scope.as_ref(), value, ui, reads, skin)
        }
    }
}

fn render_crossfader<'a>(
    path: &'a str,
    ticks: bool,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Crossfader::builder()
        .path(path)
        .ticks(ticks)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_time<'a>(
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

fn render_vis<'a>(value: Option<&ReadValue<'_>>, reads: &dyn Reads) -> Element<'a, UiEvent> {
    Vis::builder()
        .maybe_preset(value)
        .reads(reads)
        .build()
        .view()
}

fn render_deck_summary<'a>(
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

fn render_titlebar<'a>(label: InternId, ui: &'a CompiledUi, skin: &Skin) -> Element<'a, UiEvent> {
    TitleBar::builder()
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

fn render_window_controls(style: WindowControlsStyle, skin: &Skin) -> Element<'static, UiEvent> {
    WindowControls::builder()
        .style(style)
        .skin(skin)
        .build()
        .view()
}

fn render_fader<'a>(
    path: &'a str,
    style: FaderStyle,
    label: Option<&'a str>,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Fader::builder()
        .path(path)
        .style(style)
        .maybe_label(label)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_chip<'a>(
    path: &'a str,
    label: &'a str,
    style: ChipStyle,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Chip::builder()
        .path(path)
        .label(label)
        .style(style)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_knob<'a>(
    path: &'a str,
    label: Option<&'a str>,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Knob::builder()
        .path(path)
        .maybe_label(label)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

#[derive(Clone, Copy)]
struct TrackListParts<'a, 'spec, 'value, 'data> {
    path: &'a str,
    columns: &'spec [TrackColumn],
    columns_state: Option<&'spec Binding>,
    assign: &'spec [InternId],
    value: Option<&'value ReadValue<'data>>,
}

fn render_track_list<'a>(
    parts: TrackListParts<'a, '_, '_, '_>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let assign: Vec<&str> = parts.assign.iter().map(|id| ui.resolve(*id)).collect();
    TrackList::builder()
        .path(parts.path)
        .columns(parts.columns)
        .maybe_columns_state(parts.columns_state.map(|binding| ui.resolve(binding.id())))
        .columns_scope(read_scope(parts.columns_state, ui))
        .assign(assign)
        .maybe_value(parts.value)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

fn render_text<'a>(
    style: TextStyle,
    label: Option<&'a str>,
    active: bool,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Text::builder()
        .style(style)
        .maybe_value(value)
        .maybe_label(label)
        .active(active)
        .skin(skin)
        .build()
        .view()
}

fn render_button<'a>(
    path: &'a str,
    label: &'a str,
    icon: Option<IconName>,
    active_label: Option<&'a str>,
    style: ButtonStyle,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    ControlButton::builder()
        .path(path)
        .label(label)
        .maybe_icon(icon.map(render_icon))
        .maybe_active_label(active_label)
        .style(style)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_readout<'a>(
    label: Option<InternId>,
    tone: Tone,
    framed: bool,
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Readout::builder()
        .maybe_label(label.map(|id| ui.resolve(id)))
        .tone(tone)
        .framed(framed)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_tree<'a>(
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

fn read_flag(binding: Option<&Binding>, reads: &dyn Reads, ui: &CompiledUi) -> bool {
    matches!(
        binding.and_then(|binding| resolve(reads, binding, ui)),
        Some(ReadValue::Bool(true))
    )
}

fn wave_zoom(zoom: Option<&Binding>, reads: &dyn Reads, ui: &CompiledUi) -> f32 {
    zoom.and_then(|binding| resolve(reads, binding, ui))
        .and_then(|value| match value {
            ReadValue::Scalar(value) => Some(value.as_()),
            _ => None,
        })
        .unwrap_or(DEFAULT_ZOOM)
}

fn render_context_bar<'a>(
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

fn render_segmented<'a>(
    path: &'a str,
    items: &[InternId],
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Segmented::builder()
        .path(path)
        .items(items.iter().map(|id| ui.resolve(*id)).collect())
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_select<'a>(label: InternId, ui: &'a CompiledUi, skin: &Skin) -> Element<'a, UiEvent> {
    Select::builder()
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

fn render_swatch<'a>(
    role: ColorRole,
    label: InternId,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Swatch::builder()
        .role(role)
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

fn render_status_dot<'a>(
    label: InternId,
    tone: Tone,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    StatusDot::builder()
        .label(ui.resolve(label))
        .tone(tone)
        .skin(skin)
        .build()
        .view()
}

fn render_cell<'a>(
    label: Option<InternId>,
    highlighted: bool,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Cell::builder()
        .maybe_label(label.map(|id| ui.resolve(id)))
        .highlighted(highlighted)
        .skin(skin)
        .build()
        .view()
}

fn render_glyph(icon: IconName, style: GlyphStyle, skin: &Skin) -> Element<'static, UiEvent> {
    let vis = style == GlyphStyle::Vis;
    Glyph::builder()
        .icon(render_icon(icon))
        .size(if vis {
            skin.vis.icon_size
        } else {
            skin.nav.header_icon_size
        })
        .color(if vis {
            skin.color(skin.vis.icon_color)
        } else {
            skin.palette.text
        })
        .build()
        .view()
}

fn render_nav_item<'a>(
    path: &'a str,
    label: &'a str,
    icon: IconName,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    NavItem::builder()
        .path(path)
        .label(label)
        .icon(render_icon(icon))
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_tab_large<'a>(
    path: &'a str,
    label: &'a str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    TabLarge::builder()
        .path(path)
        .label(label)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn resolve<'a>(reads: &'a dyn Reads, binding: &Binding, ui: &CompiledUi) -> Option<ReadValue<'a>> {
    match binding {
        Binding::Command { .. } => None,
        binding => reads.get(ui.resolve(binding.key())),
    }
}

/// Scope suffix (`@deck=a` or empty) of a control's read binding. Widgets
/// append it to their derived endpoints so per-scope reads stay addressable.
fn read_scope<'a>(read: Option<&Binding>, ui: &'a CompiledUi) -> &'a str {
    read.map_or("", |binding| {
        let key = ui.resolve(binding.key());
        let id_len = ui.resolve(binding.id()).len();
        key.get(id_len..).unwrap_or("")
    })
}

fn effective_size(node: &ExpandedNode, skin: &Skin) -> Option<SizeSpec> {
    let declared = match node {
        ExpandedNode::Row { size, .. }
        | ExpandedNode::Column { size, .. }
        | ExpandedNode::Slot { size, .. }
        | ExpandedNode::Control { size, .. } => *size,
    };
    declared.or_else(|| match node {
        ExpandedNode::Control {
            spec: ControlSpec::TabLarge { .. },
            ..
        } => None,
        ExpandedNode::Control { spec, .. } => Some(control_size(spec, skin.document())),
        _ => None,
    })
}

fn render_icon(icon: IconName) -> Icon {
    match icon {
        IconName::ChevronDown => Icon::ChevronDown,
        IconName::ChevronUp => Icon::ChevronUp,
        IconName::Disc => Icon::Disc,
        IconName::Faders => Icon::Faders,
        IconName::FastForward => Icon::FastForward,
        IconName::Gear => Icon::Gear,
        IconName::Headphones => Icon::Headphones,
        IconName::Maximize => Icon::Maximize,
        IconName::Menu => Icon::Menu,
        IconName::Play => Icon::Play,
        IconName::PlayReverse => Icon::PlayReverse,
        IconName::Playlist => Icon::Playlist,
        IconName::Rewind => Icon::Rewind,
        IconName::SpeakerHigh => Icon::SpeakerHigh,
        IconName::Waveform => Icon::Waveform,
        IconName::X => Icon::X,
        IconName::ZoomIn => Icon::ZoomIn,
        IconName::ZoomOut => Icon::ZoomOut,
    }
}

fn render_tree_icon(icon: TreeIcon) -> Icon {
    match icon {
        TreeIcon::Collection => Icon::Collection,
        TreeIcon::Playlist => Icon::Playlist,
        TreeIcon::Folder => Icon::Folder,
        TreeIcon::Plus => Icon::Plus,
        TreeIcon::Zvuk => Icon::Zvuk,
        TreeIcon::Search => Icon::Search,
        TreeIcon::Charts => Icon::Charts,
        TreeIcon::Monitor => Icon::Monitor,
        TreeIcon::Home => Icon::Home,
        TreeIcon::Usb => Icon::Usb,
        TreeIcon::Instrument => Icon::Instrument,
        TreeIcon::Waveform => Icon::Waveform,
        TreeIcon::Clock => Icon::Clock,
    }
}

fn apply_size<'a>(
    element: Element<'a, UiEvent>,
    size: Option<SizeSpec>,
    align: Horizontal,
) -> Element<'a, UiEvent> {
    let Some(size) = size else {
        return element;
    };
    let intrinsic = element.as_widget().size_hint();
    container(element)
        .width(length_for(size.w, intrinsic.width))
        .height(length_for(size.h, intrinsic.height))
        .align_x(align)
        .align_y(Vertical::Center)
        .into()
}

/// Where a control's content sits inside the box the document gave it. Only
/// text declares this; everything else keeps the leading edge.
fn node_align(node: &ExpandedNode) -> Horizontal {
    let ExpandedNode::Control {
        spec: ControlSpec::Text { align, .. },
        ..
    } = node
    else {
        return Horizontal::Left;
    };
    match align {
        TextAlign::Start => Horizontal::Left,
        TextAlign::Center => Horizontal::Center,
        TextAlign::End => Horizontal::Right,
    }
}

fn length_for(dim: Dim, intrinsic: Length) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        Dim::Shrink => Length::Shrink,
        Dim::Range { .. } => match intrinsic {
            Length::FillPortion(_) => intrinsic,
            _ => Length::Fill,
        },
        _ => Length::Fill,
    }
}

fn child_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Split { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}

fn split_length(dim: Dim, weight: f32, skin: &Skin) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        _ => Length::FillPortion(fill_portion(weight, skin)),
    }
}

fn fill_portion(weight: f32, skin: &Skin) -> u16 {
    let scaled = (weight * skin.layout.fill_weight_scale)
        .round()
        .max(skin.layout.fill_weight_min)
        .min(f32::from(u16::MAX));
    scaled.as_()
}

#[cfg(test)]
mod tests {
    use iced::{Size, widget::Space};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        ids::{Interner, SourceUri},
        module::AdaptivePolicy,
    };

    #[kithara::test]
    fn fixed_size_spec_sets_both_element_axes() {
        let element: Element<'static, UiEvent> = Space::new().into();
        let element = apply_size(
            element,
            Some(SizeSpec::new(Dim::Fixed(34.0), Dim::Fixed(6.0))),
            Horizontal::Left,
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::Fixed(34.0), Length::Fixed(6.0))
        );
    }

    /// A shrunk axis reaches the toolkit as `Shrink` rather than as the `Fill`
    /// every other open axis resolves to.
    #[kithara::test]
    fn shrink_size_spec_reaches_the_toolkit() {
        let element: Element<'static, UiEvent> =
            Space::new().width(Length::Fill).height(Length::Fill).into();
        let element = apply_size(
            element,
            Some(SizeSpec::new(Dim::Shrink, Dim::Fill)),
            Horizontal::Left,
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::Shrink, Length::Fill)
        );
    }

    /// Both axes carry the declared rule: a container that measures its content
    /// must pass `Shrink` to its own children, or the first `Fill` inside it
    /// claims the row.
    #[kithara::test]
    fn content_size_follows_both_declared_axes() {
        let origin = SourceUri("tree-test.ron".to_owned());
        let skin = Skin::resolve(builtin::skin_doc().clone(), &origin).unwrap();
        let mut interner = Interner::new(1024);
        let id = interner.intern("cell", &origin).unwrap();
        let node = |size| ExpandedNode::Control {
            path: id,
            id,
            spec: ControlSpec::Time,
            size,
            read: None,
            write: None,
            adaptive: AdaptivePolicy::default(),
        };

        assert_eq!(
            content_size(&node(Some(SizeSpec::new(Dim::Shrink, Dim::Shrink))), &skin),
            (Length::Shrink, Length::Shrink)
        );
        assert_eq!(
            content_size(
                &node(Some(SizeSpec::new(Dim::Fixed(40.0), Dim::Shrink))),
                &skin
            ),
            (Length::Fixed(40.0), Length::Shrink)
        );
        assert_eq!(
            content_size(&node(None), &skin),
            (
                length_for(skin.document().deck.time_size.w, Length::Fill),
                length_for(skin.document().deck.time_size.h, Length::Fill)
            )
        );
    }

    #[kithara::test]
    fn range_preserves_widget_fill_portion() {
        let element: Element<'static, UiEvent> = Space::new()
            .width(Length::FillPortion(2))
            .height(Length::Fill)
            .into();
        let element = apply_size(
            element,
            Some(SizeSpec::new(
                Dim::Range {
                    min: 20.0,
                    max: None,
                },
                Dim::Fill,
            )),
            Horizontal::Left,
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::FillPortion(2), Length::Fill)
        );
    }
}
