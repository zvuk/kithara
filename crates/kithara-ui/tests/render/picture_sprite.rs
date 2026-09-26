use kithara_test_utils::kithara;
use kithara_ui::{builtin, draw::Image, render::picture::sprite::Sheet, source::SourceResolver};

/// The sheet the dark skin names, read the way a host reads it.
fn spinner() -> std::sync::Arc<[u8]> {
    builtin::resolver()
        .bytes(None, "sprites/spinner.png")
        .expect("the shipped folder holds the spinner sheet")
        .bytes
}

#[kithara::test]
fn the_builtin_sheet_cuts_into_the_frames_it_declares() {
    let sheet = Sheet::cut("spinner", &spinner(), 8, 1)
        .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

    assert_eq!(sheet.len(), 8);
}

#[kithara::test]
fn every_frame_of_the_builtin_sheet_is_square() {
    let sheet = Sheet::cut("spinner", &spinner(), 8, 1)
        .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

    for index in 0..sheet.len() {
        let Some(frame) = sheet.frame(index) else {
            panic!("frame {index} must be cut");
        };
        assert_eq!(frame.width(), frame.height(), "frame {index}");
    }
}

/// Two frames of one sheet are two pictures, not one drawn twice: a
/// rasteriser keyed on identity would otherwise show the first everywhere.
#[kithara::test]
fn two_frames_of_one_sheet_are_two_pictures() {
    let sheet = Sheet::cut("spinner", &spinner(), 8, 1)
        .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

    assert_ne!(sheet.frame(0).map(Image::id), sheet.frame(1).map(Image::id));
}

#[kithara::test]
fn frames_of_one_sheet_differ_in_their_pixels() {
    let sheet = Sheet::cut("spinner", &spinner(), 8, 1)
        .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

    assert_ne!(
        sheet.frame(0).and_then(Image::rgba),
        sheet.frame(4).and_then(Image::rgba)
    );
}

#[kithara::test]
fn a_running_index_wraps_back_to_the_first_frame() {
    let sheet = Sheet::cut("spinner", &spinner(), 8, 1)
        .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

    assert_eq!(sheet.frame(8).map(Image::id), sheet.frame(0).map(Image::id));
}

#[kithara::test]
fn a_grid_that_does_not_divide_the_sheet_is_refused() {
    assert!(Sheet::cut("spinner", &spinner(), 7, 1).is_err());
}

#[kithara::test]
fn something_that_is_not_a_png_is_refused() {
    assert!(Sheet::cut("nonsense", b"not a png", 1, 1).is_err());
}
