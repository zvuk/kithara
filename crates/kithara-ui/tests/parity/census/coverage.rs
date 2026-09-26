use kithara_test_utils::kithara;
use kithara_ui::{
    compile::{CompiledNode, CompiledUi},
    expand::{ControlSpec, ExpandedNode},
};

use super::{fixture::Fixture, named::ROWS, table::CONTROL_CENSUS};

/// The first control under a node, looking through everything that only
/// holds one.
fn find_control(node: &ExpandedNode) -> Option<&ControlSpec> {
    match node {
        ExpandedNode::Control { spec, .. } => Some(spec),
        ExpandedNode::Object { child, .. }
        | ExpandedNode::Optional { child, .. }
        | ExpandedNode::Placed { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Reveal { child, .. }
        | ExpandedNode::Scroll { child, .. } => find_control(child),
        ExpandedNode::Adaptive { base, steps, .. } => {
            find_control(base).or_else(|| steps.iter().find_map(|(_, branch)| find_control(branch)))
        }
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. }
        | ExpandedNode::Stage { children, .. } => children.iter().find_map(find_control),
        ExpandedNode::Popover {
            anchor, content, ..
        } => find_control(anchor).or_else(|| find_control(content)),
        _ => None,
    }
}

/// The one control a census document mounts.
fn compiled_control(ui: &CompiledUi) -> &ControlSpec {
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("the census fixture must compile to one module");
    };
    find_control(root).unwrap_or_else(|| panic!("the census fixture must contain a control"))
}

/// A census short by a control agrees with the other census, which is short
/// by the same one, and neither reports a gap. Only the document contract
/// can say what the full set is.
#[kithara::test]
fn the_census_covers_every_control_the_document_can_name() {
    let mut censused = CONTROL_CENSUS
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<Vec<_>>();
    let mut declared = ControlSpec::KINDS.to_vec();
    censused.sort_unstable();
    declared.sort_unstable();

    assert_eq!(
        censused, declared,
        "every `ControlSpec` variant needs a census row saying what it draws"
    );
}

/// Each gesture row stands beside the paint row of the same control, and
/// that row's fixture mounts the control both rows name.
#[kithara::test]
fn every_gesture_row_mounts_the_control_it_names() {
    let painted = CONTROL_CENSUS
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<Vec<_>>();
    let gestured = ROWS.iter().map(|row| row.name).collect::<Vec<_>>();
    assert_eq!(
        gestured, painted,
        "the paint and gesture censuses must cover the same controls in the same order"
    );

    for (row, (_, _, control)) in ROWS.iter().zip(CONTROL_CENSUS) {
        let ui = Fixture::new(control).compiled();
        let spec = compiled_control(&ui);
        assert_eq!(
            spec.kind(),
            row.name,
            "the census row for {} mounts a different control than it names",
            row.name
        );
    }
}
