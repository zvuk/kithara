use iced::{
    Size,
    advanced::{
        layout::{Layout, Limits},
        widget::Tree,
    },
};
use kithara_test_utils::kithara;
use kithara_ui::{
    draw::Rect,
    render::{Clock, Skin, tree},
    view,
};
use num_traits::cast::AsPrimitive;

use super::{
    fixture::{CONTROL, CensusReads, Fixture, HEIGHT, WIDTH, skin},
    table::CONTROL_CENSUS,
};
use crate::shared::{renderer, snapped};

/// The box the retained host laid a control into.
fn retained_box(control: &str, skin: &Skin) -> Rect {
    let fixture = Fixture::new(control);
    fixture
        .mount(skin)
        .rect_of(CONTROL)
        .unwrap_or_else(|| panic!("the retained host must mount {control} as a leaf"))
}

/// The box the immediate host laid the same control into.
///
/// The hosts disagree on how many nodes a control is: the retained one
/// mounts a single widget carrying the resolved size, and the immediate one
/// wraps the control's own element in a container of that size. Descending
/// through every node that stands alone reaches the surface both hosts
/// paint, and stopping at the first node that splits keeps a control that
/// lays out children - only `Tree` does - measured as the surface they are
/// painted on.
fn immediate_box(control: &str, skin: &Skin) -> Rect {
    let ui = Fixture::new(control).compiled();
    let reads = CensusReads;
    let mut element = tree::render(
        &ui.root,
        &ui,
        &reads,
        &view::EMPTY,
        skin,
        Clock::default(),
        None,
    );
    let mut state = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut state,
        &renderer(),
        &Limits::new(Size::ZERO, Size::new(WIDTH.as_(), HEIGHT.as_())),
    );
    let mut layout = Layout::new(&node);
    loop {
        let mut children = layout.children();
        let Some(only) = children.next() else { break };
        if children.next().is_some() {
            break;
        }
        layout = only;
    }
    let bounds = layout.bounds();
    Rect {
        x: bounds.x,
        y: bounds.y,
        w: bounds.width,
        h: bounds.height,
    }
}

/// A button given a box paints all of it, on both hosts.
///
/// Every `Button` a shipped document names declares a size, and the census
/// above cannot see that shape: it drives each control as the document
/// leaves it. The retained host mounts one widget of the declared box; the
/// immediate host wraps the button's own element in a container of that box
/// and lets the element ask for a width of its own, so a button that asks
/// for the width of its word is painted narrower than the box the document
/// gave it.
#[kithara::test]
fn a_button_given_a_box_paints_all_of_it_on_both_hosts() {
    let skin = skin();
    let control = r#"Button(id: "control", label: "PLAY", size: (w: Fixed(72.0), h: Fixed(28.0)), read: Model(id: "ui.menu.open"))"#;

    assert_eq!(
        snapped(immediate_box(control, &skin)),
        snapped(retained_box(control, &skin)),
        "the two hosts paint a button given the same box differently"
    );
}

/// Both hosts lay the same control into the same box.
///
/// A declared size reaches the two hosts through separate tables -
/// `length_for` on the immediate one, `control_length` on the retained one
/// - and a control whose painter measures its own width is where the two
/// can part: one gives the parent the painter's box and the other replaces
/// it with the skin's. No shipped document names such a size, so only a
/// census over every control keeps the two tables answering alike.
#[kithara::test]
fn every_control_is_laid_out_into_the_same_box_on_both_hosts() {
    let skin = skin();

    let mut retained = Vec::new();
    let mut immediate = Vec::new();
    for (name, _, control) in CONTROL_CENSUS {
        retained.push(format!(
            "{name}: {:?}",
            snapped(retained_box(control, &skin))
        ));
        immediate.push(format!(
            "{name}: {:?}",
            snapped(immediate_box(control, &skin))
        ));
    }

    assert_eq!(
        retained, immediate,
        "the two hosts lay the same control into different boxes"
    );
}
