use kithara_test_utils::kithara;
use kithara_ui_draw::{DrawCmd, DrawListBuilder, Geom, Paint};
use kithara_ui_lottie::{builtin_artwork, emit};
use velato::Composition;

/// One square, filled, under one group transform: the smallest artwork that
/// exercises the whole walk — layer, group, contour, draw.
const PROBE: &str = include_str!("probe.json");

fn read() -> Composition {
    Composition::from_slice(PROBE.as_bytes())
        .unwrap_or_else(|error| panic!("the probe artwork must read: {error}"))
}

fn drawn() -> Vec<DrawCmd> {
    let mut list = DrawListBuilder::default();
    emit(&read(), 0.0, &mut list)
        .unwrap_or_else(|error| panic!("the probe artwork must draw: {error}"));
    list.finish().commands().to_vec()
}

/// Three layers, each one draw: the plate, the rule and the ramp.
#[kithara::test]
fn every_layer_of_the_probe_is_drawn() {
    assert_eq!(drawn().len(), 3);
}

/// The rectangle and the disc share a transform with no draw between them,
/// so they merge into one contour that a single fill claims — the merge
/// rule is what decides how many shapes one draw covers.
#[kithara::test]
fn two_shapes_under_one_fill_become_one_contour() {
    assert_eq!(
        drawn()
            .iter()
            .filter(|command| matches!(command, DrawCmd::Fill { .. }))
            .count(),
        2
    );
}

#[kithara::test]
fn a_contour_reaches_the_list_as_an_outline_rather_than_a_named_shape() {
    assert!(drawn().iter().all(|command| matches!(
        command,
        DrawCmd::Fill {
            geom: Geom::Path(_),
            ..
        } | DrawCmd::Stroke {
            geom: Geom::Path(_),
            ..
        }
    )));
}

/// The plate layer is drawn at four fifths, and its own fill at full, so
/// the colour that reaches the list carries the layer's opacity — velato
/// folds it into the brush rather than opening a layer for it. The plate is
/// the artwork's last layer, and a Lottie is drawn back to front, so it is
/// the first thing this list says.
#[kithara::test]
fn a_layers_opacity_is_folded_into_the_colour_it_paints_with() {
    let drawn = drawn();
    let Some(DrawCmd::Fill {
        paint: Paint::Solid(color),
        ..
    }) = drawn.first()
    else {
        panic!("the plate is the first thing drawn, and it is one colour");
    };

    assert!((color.a - 0.8).abs() < 1e-3, "alpha was {}", color.a);
}

#[kithara::test]
fn a_ramp_the_artwork_names_reaches_the_list_as_a_ramp() {
    assert!(drawn().iter().any(|command| matches!(
        command,
        DrawCmd::Fill {
            paint: Paint::Linear { .. },
            ..
        }
    )));
}

/// The artwork the gallery shows. Three layers keyframed over two seconds:
/// two turning opposite ways and one breathing. Nothing here reads a clock,
/// so a frame drawn twice is drawn the same, and two frames apart are only
/// the same if the artwork does not move — which is what this asks.
#[kithara::test]
fn the_shipped_artwork_draws_a_different_picture_at_a_different_frame() {
    let artwork = builtin_artwork("pulse").expect("the toolkit ships the pulse artwork");
    let at = |frame: f64| {
        let mut list = DrawListBuilder::default();
        emit(artwork.composition(), frame, &mut list)
            .unwrap_or_else(|error| panic!("the shipped artwork must draw: {error}"));
        list.finish().commands().to_vec()
    };

    assert_ne!(at(0.0), at(30.0));
}

#[kithara::test]
fn a_stroked_contour_reaches_the_list_as_a_stroke() {
    assert!(
        drawn()
            .iter()
            .any(|command| matches!(command, DrawCmd::Stroke { .. }))
    );
}
