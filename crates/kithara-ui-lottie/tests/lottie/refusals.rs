//! What an artwork asks for that this vocabulary has no word for.

use kithara_test_utils::kithara;
use kithara_ui_draw::DrawListBuilder;
use kithara_ui_lottie::{LottieError, emit};
use velato::Composition;

/// One square with half its contour trimmed away, which is the shape the
/// emitter does not cut. Everything else about it is drawable, so what is
/// refused is the one thing named.
///
/// A trim rather than a repeater because velato imports only the first of
/// the two: its repeater arm is commented out, so no document can put a
/// `Shape::Repeater` in front of this emitter.
fn refused() -> (Result<(), LottieError>, usize) {
    const TRIMMED: &str = r#"{
    "v": "5.7.0", "fr": 60, "ip": 0, "op": 60, "w": 100, "h": 100, "nm": "trimmed",
    "ddd": 0, "assets": [],
    "layers": [{
        "ddd": 0, "ind": 1, "ty": 4, "nm": "cut", "sr": 1, "ao": 0,
        "ip": 0, "op": 60, "st": 0, "bm": 0,
        "ks": {
            "o": {"a": 0, "k": 100}, "r": {"a": 0, "k": 0},
            "p": {"a": 0, "k": [50, 50, 0]}, "a": {"a": 0, "k": [0, 0, 0]},
            "s": {"a": 0, "k": [100, 100, 100]}
        },
        "shapes": [{"ty": "gr", "nm": "group", "it": [
            {"ty": "rc", "nm": "square", "d": 1,
             "s": {"a": 0, "k": [20, 20]}, "p": {"a": 0, "k": [0, 0]}, "r": {"a": 0, "k": 0}},
            {"ty": "fl", "nm": "fill", "r": 1,
             "c": {"a": 0, "k": [1, 1, 1, 1]}, "o": {"a": 0, "k": 100}},
            {"ty": "tm", "nm": "trim", "m": 1,
             "s": {"a": 0, "k": 0}, "e": {"a": 0, "k": 50}, "o": {"a": 0, "k": 0}},
            {"ty": "tr", "p": {"a": 0, "k": [0, 0]}, "a": {"a": 0, "k": [0, 0]},
             "s": {"a": 0, "k": [100, 100]}, "r": {"a": 0, "k": 0}, "o": {"a": 0, "k": 100}}
        ]}]
    }]
    }"#;

    let artwork = Composition::from_slice(TRIMMED.as_bytes())
        .unwrap_or_else(|error| panic!("the trimmed artwork must read: {error}"));
    let mut list = DrawListBuilder::default();
    let refusal = emit(&artwork, 0.0, &mut list);

    (refusal, list.finish().commands().len())
}

#[kithara::test]
fn what_the_emitter_cannot_cut_is_refused_by_name() {
    assert!(matches!(refused().0, Err(LottieError::Modifier { .. })));
}

/// The whole artwork, not the shape it stumbled on: a picture that half
/// draws is one nobody authored.
#[kithara::test]
fn a_refused_artwork_leaves_the_list_empty() {
    assert_eq!(refused().1, 0);
}
