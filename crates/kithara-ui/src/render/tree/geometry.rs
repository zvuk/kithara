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
    pub(super) fn new(element: Element<'a, UiEvent>, align: Horizontal) -> Self {
        Self { element, align }
    }

    pub(super) fn leading(element: Element<'a, UiEvent>) -> Self {
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
    size: (Length, Length),
    skin: &Skin,
) -> Element<'a, UiEvent> {
    match frame {
        Some(sides) => frame_overlay(element, sides, size, skin),
        None => element,
    }
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
        ExpandedNode::Optional { child, .. } => return effective_size(child, skin),
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

pub(super) fn length_for(dim: Dim, intrinsic: Length) -> Length {
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
