use iced::{
    Background, Color, Element, Length, Padding,
    alignment::{Horizontal, Vertical},
    widget::{Container, container, container::Style as ContainerStyle},
};

use crate::{
    expand::{ControlSpec, ExpandedNode},
    layout::FrameSides,
    render::{Skin, UiEvent},
    size::{Dim, SizeSpec, control_size},
    skin::ColorRole,
    widgets::frame_overlay,
};

pub(super) struct Rendered<'a> {
    element: Element<'a, UiEvent>,
    align: Horizontal,
}

impl<'a> Rendered<'a> {
    pub(super) const fn new(element: Element<'a, UiEvent>, align: Horizontal) -> Self {
        Self { element, align }
    }

    pub(super) const fn leading(element: Element<'a, UiEvent>) -> Self {
        Self::new(element, Horizontal::Left)
    }
}

pub(super) fn padding(
    pad: Option<f32>,
    pad_x: Option<f32>,
    pad_y: Option<f32>,
    skin: &Skin,
) -> Padding {
    let base = pad.unwrap_or(skin.layout.grid_pad);
    Padding::ZERO
        .top(pad_y.unwrap_or(base))
        .bottom(pad_y.unwrap_or(base))
        .left(pad_x.unwrap_or(base))
        .right(pad_x.unwrap_or(base))
}

pub(super) fn filled<'a>(
    element: Container<'a, UiEvent>,
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

pub(super) fn bordered<'a>(
    element: Element<'a, UiEvent>,
    frame: Option<FrameSides>,
    tone: (ColorRole, f32),
    size: (Length, Length),
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let (role, width) = tone;
    match frame {
        Some(sides) => frame_overlay(element, sides, size, skin.color(role), width),
        None => element,
    }
}

pub(crate) fn active_tone(
    base: Option<ColorRole>,
    active: Option<ColorRole>,
    on: bool,
) -> Option<ColorRole> {
    on.then_some(active).flatten().or(base)
}

pub(super) fn frame_tone(
    frame_color: Option<ColorRole>,
    active_frame_color: Option<ColorRole>,
    active: bool,
    skin: &Skin,
) -> (ColorRole, f32) {
    (
        active_tone(frame_color, active_frame_color, active).unwrap_or(skin.divider.color),
        skin.divider.width,
    )
}

pub(super) fn content_size(node: &ExpandedNode, skin: &Skin) -> (Length, Length) {
    effective_size(node, skin).map_or((Length::Fill, Length::Fill), |size| {
        (
            length_for(size.w, Length::Fill),
            length_for(size.h, Length::Fill),
        )
    })
}

pub(super) fn effective_size(node: &ExpandedNode, skin: &Skin) -> Option<SizeSpec> {
    let declared = match node {
        ExpandedNode::Optional { child, .. } | ExpandedNode::Pressable { child, .. } => {
            return effective_size(child, skin);
        }
        ExpandedNode::Popover { anchor, .. } => return effective_size(anchor, skin),
        ExpandedNode::Scroll { size, .. }
        | ExpandedNode::Row { size, .. }
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

pub(super) fn apply_size<'a>(
    rendered: Rendered<'a>,
    size: Option<SizeSpec>,
) -> Element<'a, UiEvent> {
    let Rendered { element, align } = rendered;
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

pub(super) const fn length_for(dim: Dim, intrinsic: Length) -> Length {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use iced::{Size, widget::Space};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        expand::{Binding, BindingKind},
        ids::{InternId, Interner, SourceUri},
        module::{PopoverAlign, PopoverAt},
    };

    #[kithara::test]
    fn fixed_size_spec_sets_both_element_axes() {
        let element: Element<'static, UiEvent> = Space::new().into();
        let element = apply_size(
            Rendered::leading(element),
            Some(SizeSpec::new(Dim::Fixed(34.0), Dim::Fixed(6.0))),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::Fixed(34.0), Length::Fixed(6.0))
        );
    }

    #[kithara::test]
    fn shrink_size_spec_reaches_the_toolkit() {
        let element: Element<'static, UiEvent> =
            Space::new().width(Length::Fill).height(Length::Fill).into();
        let element = apply_size(
            Rendered::leading(element),
            Some(SizeSpec::new(Dim::Shrink, Dim::Fill)),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::Shrink, Length::Fill)
        );
    }

    #[kithara::test]
    fn content_size_follows_both_declared_axes() {
        let origin = SourceUri("tree-test.ron".to_owned());
        let skin =
            Skin::resolve(builtin::skin_doc().clone(), builtin::text_doc(), &origin).unwrap();
        let mut interner = Interner::new(1024);
        let id = interner.intern("cell", &origin).unwrap();
        let node = |size| ExpandedNode::Control {
            path: id,
            id,
            spec: ControlSpec::Time,
            size,
            read: None,
            write: None,
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

    fn control(
        interner: &mut Interner,
        origin: &SourceUri,
        name: &str,
        size: SizeSpec,
    ) -> ExpandedNode {
        let id = interner.intern(name, origin).unwrap();
        ExpandedNode::Control {
            path: id,
            id,
            spec: ControlSpec::Time,
            size: Some(size),
            read: None,
            write: None,
        }
    }

    fn model(id: InternId) -> Binding {
        Binding {
            kind: BindingKind::Model,
            id,
            key: id,
            with: BTreeMap::new(),
        }
    }

    #[kithara::test]
    fn a_popover_measures_its_anchor_and_a_pressable_its_child() {
        let origin = SourceUri("tree-test.ron".to_owned());
        let skin =
            Skin::resolve(builtin::skin_doc().clone(), builtin::text_doc(), &origin).unwrap();
        let mut interner = Interner::new(1024);
        let anchor = SizeSpec::new(Dim::Fixed(36.0), Dim::Fixed(36.0));
        let content = SizeSpec::new(Dim::Fixed(298.0), Dim::Fixed(400.0));
        let popover = ExpandedNode::Popover {
            path: interner.intern("menu", &origin).unwrap(),
            open: model(interner.intern("ui.menu.open", &origin).unwrap()),
            at: PopoverAt::Anchor,
            align: PopoverAlign::Start,
            anchor: Box::new(control(&mut interner, &origin, "burger", anchor)),
            content: Box::new(control(&mut interner, &origin, "pop", content)),
        };
        let pressable = ExpandedNode::Pressable {
            path: interner.intern("row", &origin).unwrap(),
            press: model(interner.intern("ui.menu.toggle", &origin).unwrap()),
            child: Box::new(control(&mut interner, &origin, "cell", content)),
        };

        assert_eq!(
            effective_size(&popover, &skin),
            Some(anchor),
            "the content is laid out in the overlay and never in flow"
        );
        assert_eq!(effective_size(&pressable, &skin), Some(content));
    }

    #[kithara::test]
    fn active_tone_takes_the_active_role_only_while_the_flag_is_set() {
        let pair =
            |active| active_tone(Some(ColorRole::LineInner), Some(ColorRole::Accent), active);

        assert_eq!(pair(true), Some(ColorRole::Accent));
        assert_eq!(pair(false), Some(ColorRole::LineInner));
        assert_eq!(
            active_tone(Some(ColorRole::LineHi), None, true),
            Some(ColorRole::LineHi)
        );
        assert_eq!(active_tone(None, None, true), None);
    }

    #[kithara::test]
    fn a_node_naming_no_frame_colour_takes_the_skin_divider() {
        let skin = builtin::skin();

        assert_eq!(
            frame_tone(None, None, false, skin),
            (skin.divider.color, skin.divider.width)
        );
    }

    #[kithara::test]
    fn a_declared_frame_pair_switches_on_the_active_flag() {
        let skin = builtin::skin();
        let pair = |active| {
            frame_tone(
                Some(ColorRole::LineInner),
                Some(ColorRole::Accent),
                active,
                skin,
            )
        };

        assert_eq!(pair(true), (ColorRole::Accent, skin.divider.width));
        assert_eq!(pair(false), (ColorRole::LineInner, skin.divider.width));
        assert_eq!(
            frame_tone(Some(ColorRole::LineHi), None, true, skin),
            (ColorRole::LineHi, skin.divider.width)
        );
    }

    #[kithara::test]
    fn range_preserves_widget_fill_portion() {
        let element: Element<'static, UiEvent> = Space::new()
            .width(Length::FillPortion(2))
            .height(Length::Fill)
            .into();
        let element = apply_size(
            Rendered::leading(element),
            Some(SizeSpec::new(
                Dim::Range {
                    min: 20.0,
                    max: None,
                },
                Dim::Fill,
            )),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::FillPortion(2), Length::Fill)
        );
    }
}
