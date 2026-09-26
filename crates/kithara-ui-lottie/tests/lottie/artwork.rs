use kithara_test_utils::kithara;
use kithara_ui_lottie::{Artwork, builtin_artwork};

fn shipped() -> &'static Artwork {
    builtin_artwork("pulse").expect("the toolkit ships the pulse artwork")
}

/// A document that switches artwork on a flag needs two that read, and two
/// that are not the same drawing.
#[kithara::test]
fn the_two_shipped_artworks_are_two_drawings() {
    let spark = builtin_artwork("spark").expect("the toolkit ships the spark artwork");

    assert!(!std::ptr::eq(shipped(), spark));
}

#[kithara::test]
fn an_artwork_the_toolkit_does_not_ship_is_not_found() {
    assert!(builtin_artwork("nothing-of-the-sort").is_none());
}

/// The whole point of a pass: a clock that keeps running keeps playing,
/// rather than stopping on the last frame it reached.
#[kithara::test]
fn a_reading_a_whole_pass_later_comes_back_to_the_same_frame() {
    let artwork = shipped();

    assert_eq!(artwork.frame_at(0.0, 2.0), artwork.frame_at(2.0, 2.0));
}

#[kithara::test]
fn a_reading_partway_through_a_pass_stands_at_a_later_frame() {
    let artwork = shipped();

    assert!(artwork.frame_at(1.0, 2.0) > artwork.frame_at(0.0, 2.0));
}

/// A pass of nothing holds the artwork's own first frame rather than
/// dividing by it.
#[kithara::test]
fn a_pass_of_no_time_holds_the_first_frame() {
    let artwork = shipped();

    assert_eq!(
        artwork.frame_at(1.0, 0.0),
        artwork.composition().frames.start
    );
}
