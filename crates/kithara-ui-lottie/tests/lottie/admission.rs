//! Public admission of caller-owned artwork, before any frame is drawn.

use kithara_test_utils::kithara;
use kithara_ui_draw::{DrawListBuilder, Rect, Rgba};
use kithara_ui_lottie::{Artwork, ArtworkError, emit};
use serde_json::{Value, json};
use velato::{
    Composition,
    model::{Content, Shape},
};

fn document() -> Value {
    serde_json::from_str(include_str!("probe.json"))
        .expect("the neutral artwork fixture must parse")
}

fn prefix() -> DrawListBuilder {
    let mut list = DrawListBuilder::default();
    list.fill_rect(
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        },
        Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
    );
    list
}

#[kithara::test]
fn unsupported_content_between_sampled_frames_is_refused_at_admission() {
    let mut document = document();
    let layer = &mut document["layers"][0];
    layer["ip"] = json!(10);
    layer["op"] = json!(20);
    layer["bm"] = json!(1);
    let composition = Composition::from_json(document.clone()).unwrap();
    for frame in [0.0, 30.0, 59.94] {
        assert!(emit(&composition, frame, &mut DrawListBuilder::default()).is_ok());
    }
    assert!(emit(&composition, 15.0, &mut DrawListBuilder::default()).is_err());
    assert!(Artwork::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[kithara::test]
fn an_unsupported_asset_returns_an_error_instead_of_panicking() {
    let mut document = document();
    document["assets"] = json!([{
        "id": "image", "w": 10, "h": 10, "u": "", "p": "image.png", "e": 0
    }]);
    assert!(Artwork::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[kithara::test]
fn excessive_shape_nesting_is_refused_before_import() {
    let mut document = document();
    let mut shapes = document["layers"][0]["shapes"].take();
    for _ in 0..33 {
        shapes = json!([{"ty": "gr", "it": shapes}]);
    }
    document["layers"][0]["shapes"] = shapes;
    assert!(Composition::from_json(document.clone()).is_ok());
    assert!(Artwork::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[kithara::test]
fn numbers_that_overflow_the_draw_representation_are_refused() {
    let mut document = document();
    document["layers"][0]["ks"]["p"]["k"][0] = json!(1e100);
    assert!(Composition::from_json(document.clone()).is_ok());
    assert!(Artwork::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[kithara::test]
fn cyclic_and_missing_parent_layers_are_refused() {
    let mut cyclic = document();
    cyclic["layers"][0]["parent"] = json!(2);
    cyclic["layers"][1]["parent"] = json!(1);
    assert!(Artwork::parse(&serde_json::to_vec(&cyclic).unwrap()).is_err());
    cyclic["layers"][0]["parent"] = json!(99);
    assert!(Artwork::parse(&serde_json::to_vec(&cyclic).unwrap()).is_err());
}

#[kithara::test]
fn byte_and_collection_bounds_return_typed_limit_errors() {
    assert!(matches!(
        Artwork::parse(&vec![b' '; 128 * 1024 + 1]),
        Err(ArtworkError::Limit { .. })
    ));
    let mut document = document();
    document["metadata"] = json!(vec![0; 16 * 1024]);
    let bytes = serde_json::to_vec(&document).unwrap();
    assert!(bytes.len() < 128 * 1024);
    assert!(matches!(
        Artwork::parse(&bytes),
        Err(ArtworkError::Limit { .. })
    ));
}

#[kithara::test]
fn invalid_gradient_offsets_and_incomplete_paths_are_refused() {
    let mut gradient = document();
    gradient["layers"][0]["shapes"][0]["it"][1]["g"]["k"]["k"][0] = json!(-0.5);
    assert!(matches!(
        Artwork::parse(&serde_json::to_vec(&gradient).unwrap()),
        Err(ArtworkError::Invalid { .. })
    ));
    let mut path = document();
    path["layers"][1]["shapes"][0]["it"][0]["ks"]["k"]["i"] = json!([]);
    assert!(matches!(
        Artwork::parse(&serde_json::to_vec(&path).unwrap()),
        Err(ArtworkError::Invalid { .. })
    ));
}

#[kithara::test]
fn a_gradient_without_colour_data_returns_a_typed_error() {
    let mut document = document();
    document["layers"][0]["shapes"][0]["it"][1]["g"] = json!({"p": 2});
    assert!(matches!(
        Artwork::parse(&serde_json::to_vec(&document).unwrap()),
        Err(ArtworkError::Invalid { .. })
    ));
}

#[kithara::test]
#[case::absent(Value::Null)]
#[case::split(json!({
    "x": {"a": 0, "k": 0}, "y": {"a": 0, "k": 0},
    "z": {"a": 0, "k": 0}, "or": {"a": 0, "k": [0, 0, 0]}
}))]
fn unsupported_layer_rotation_returns_an_error_instead_of_panicking(#[case] rotation: Value) {
    let mut document = document();
    document["layers"][0]["ks"]["r"] = rotation;
    assert!(Artwork::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[kithara::test]
fn nonfinite_frames_cannot_silently_become_an_empty_picture() {
    let composition = Composition::from_json(document()).unwrap();
    for frame in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut list = prefix();
        assert!(emit(&composition, frame, &mut list).is_err());
        assert_eq!(list.finish(), prefix().finish());
    }
}

#[kithara::test]
fn a_computed_transform_overflow_is_refused_before_drawing() {
    let mut document = document();
    document["layers"][0]["ks"]["s"]["k"][0] = json!(1e30);
    document["layers"][0]["shapes"][0]["it"][2]["s"]["k"][0] = json!(1e30);
    let composition = Composition::from_json(document.clone()).unwrap();
    let layer = &composition.layers[0];
    let Content::Shape(shapes) = &layer.content else {
        panic!("shape fixture");
    };
    let Shape::Group(_, Some(group)) = &shapes[0] else {
        panic!("transformed group fixture");
    };
    let composed =
        layer.transform.evaluate(0.0).into_owned() * group.transform.evaluate(0.0).into_owned();
    assert!(
        composed
            .as_coeffs()
            .iter()
            .any(|value| value.abs() > f64::from(f32::MAX))
    );
    let mut list = prefix();
    assert!(emit(&composition, 0.0, &mut list).is_err());
    assert_eq!(list.finish(), prefix().finish());
    assert!(Artwork::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[kithara::test]
#[case::expression(json!({"x": "value + [1, 0, 0]"}))]
#[case::split(json!({"s": true, "x": {"a": 0, "k": 1}, "y": {"a": 0, "k": 2}}))]
fn ignored_position_features_are_refused_instead_of_losing_authored_motion(#[case] patch: Value) {
    let mut document = document();
    document["layers"][0]["ks"]["p"]
        .as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    assert!(Composition::from_json(document.clone()).is_ok());
    assert!(matches!(
        Artwork::parse(&serde_json::to_vec(&document).unwrap()),
        Err(ArtworkError::Unsupported { .. })
    ));
}

#[kithara::test]
#[case::zero_rate("fr", 0)]
#[case::empty_width("w", 0)]
#[case::empty_height("h", 0)]
#[case::empty_frames("op", 0)]
fn invalid_composition_geometry_or_timing_is_refused(#[case] field: &str, #[case] value: i32) {
    let mut document = document();
    document[field] = json!(value);
    assert!(Composition::from_json(document.clone()).is_ok());
    assert!(Artwork::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}
