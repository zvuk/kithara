use kithara_test_utils::kithara;
use kithara_ui_draw::{
    Caps, DrawListBuilder, FillRule, Image, ImageId, Needs, Paint, Path, Pt, Rect, Rgba, Stop,
    Stops, Unsupported, Verb,
};

fn ink() -> Rgba {
    Rgba {
        a: 1.0,
        b: 1.0,
        g: 1.0,
        r: 1.0,
    }
}

fn unit_box() -> Rect {
    Rect {
        h: 10.0,
        w: 10.0,
        x: 0.0,
        y: 0.0,
    }
}

fn outline() -> Path {
    Path::new(
        FillRule::NonZero,
        vec![Verb::MoveTo(Pt { x: 0.0, y: 0.0 }), Verb::Close],
    )
}

fn stops() -> Stops {
    Stops::new(&[
        Stop {
            color: ink(),
            offset: 0.0,
        },
        Stop {
            color: ink(),
            offset: 1.0,
        },
    ])
    .unwrap_or_else(|error| panic!("a two-stop ramp is valid: {error}"))
}

fn ramp() -> Paint {
    Paint::Linear {
        from: Pt { x: 0.0, y: 0.0 },
        stops: stops(),
        to: Pt { x: 1.0, y: 0.0 },
    }
}

fn radial() -> Paint {
    Paint::Radial {
        center: Pt { x: 5.0, y: 5.0 },
        radius: 5.0,
        stops: stops(),
    }
}

/// A list of plain filled rectangles asks nothing of anyone.
#[kithara::test]
fn a_plain_list_needs_nothing_in_particular() {
    let mut list = DrawListBuilder::default();
    list.fill_rect(unit_box(), ink());

    assert_eq!(Needs::from(&list.finish()), Needs::default());
}

#[kithara::test]
fn an_image_is_refused_by_a_backend_without_an_image_registry() {
    let mut list = DrawListBuilder::default();
    list.image(
        Image::external(ImageId::new("shader/test"), 1, 1),
        unit_box(),
    );
    let needs = Needs::from(&list.finish());

    assert_eq!(Caps::EVERYTHING.accepts(needs), Ok(()));
    assert_eq!(
        Caps {
            can_draw_images: false,
            ..Caps::EVERYTHING
        }
        .accepts(needs),
        Err(Unsupported::Image)
    );
}

/// What a list needs is read from what it draws, however deeply nested.
#[kithara::test]
fn a_need_inside_a_clip_still_counts() {
    let mut inner = DrawListBuilder::default();
    inner.fill_path(outline(), ink());
    let mut list = DrawListBuilder::default();
    list.clip(unit_box(), inner.finish());
    let needs = Needs::from(&list.finish());

    assert_eq!(Caps::EVERYTHING.accepts(needs), Ok(()));
    assert_eq!(
        Caps {
            clip: false,
            ..Caps::EVERYTHING
        }
        .accepts(needs),
        Err(Unsupported::Clip)
    );
    assert_eq!(
        Caps {
            outline: false,
            ..Caps::EVERYTHING
        }
        .accepts(needs),
        Err(Unsupported::Outline)
    );
}

/// A backend without gradients refuses the document rather than flattening
/// the ramp, which would be a different picture nobody asked for.
#[kithara::test]
fn a_ramp_is_refused_where_it_cannot_be_drawn() {
    let mut list = DrawListBuilder::default();
    list.fill_rect(unit_box(), ramp());
    let needs = Needs::from(&list.finish());

    assert_eq!(Caps::EVERYTHING.accepts(needs), Ok(()));
    assert_eq!(
        Caps {
            linear_gradient: false,
            ..Caps::EVERYTHING
        }
        .accepts(needs),
        Err(Unsupported::LinearGradient)
    );
}

/// The two kinds of ramp are asked for separately, so a backend with one of
/// them is not credited with the other.
#[kithara::test]
fn a_radial_ramp_is_its_own_question() {
    let mut list = DrawListBuilder::default();
    list.fill_rect(unit_box(), radial());
    let needs = Needs::from(&list.finish());

    assert_eq!(Caps::EVERYTHING.accepts(needs), Ok(()));
    assert_eq!(
        Caps {
            linear_gradient: false,
            ..Caps::EVERYTHING
        }
        .accepts(needs),
        Ok(())
    );
    assert_eq!(
        Caps {
            radial_gradient: false,
            ..Caps::EVERYTHING
        }
        .accepts(needs),
        Err(Unsupported::RadialGradient)
    );
}
