use kithara_test_utils::kithara;

use super::tree::*;
use crate::module::TextAlign;

fn form(gap: f32) -> ExpandedNode {
    ExpandedNode::Row {
        gap: Some(gap),
        id: None,
        size: None,
        align: TextAlign::default(),
        measure: None,
        pad: None,
        pad_x: None,
        pad_y: None,
        frame: None,
        background: None,
        background_alpha: None,
        active: None,
        active_background: None,
        frame_color: None,
        active_frame_color: None,
        surface: None,
        children: Vec::new(),
    }
}

#[kithara::test]
fn a_measure_takes_the_last_step_it_reaches() {
    let base = form(0.0);
    let steps = vec![(4.0, form(4.0)), (8.0, form(8.0))];
    let branch = |value| adaptive_branch(&base, &steps, value);

    assert_eq!(branch(None), &base, "nothing read");
    assert_eq!(branch(Some(3.9)), &base, "below the first step");
    assert_eq!(branch(Some(4.0)), &steps[0].1, "exactly on a threshold");
    assert_eq!(branch(Some(7.9)), &steps[0].1);
    assert_eq!(branch(Some(8.0)), &steps[1].1);
    assert_eq!(branch(Some(f32::MAX)), &steps[1].1, "above every step");
    assert_eq!(branch(Some(f32::NAN)), &base, "ordered against nothing");
}
