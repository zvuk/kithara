use kithara_test_utils::kithara;
use kithara_ui_draw::{
    DrawCmd, DrawList, DrawListBuilder, Geom, LineCap, LineJoin, Paint, Pen, Pt, Rect, Rgba, Stop,
    Stops, Transform,
};

const fn ink() -> Rgba {
    Rgba {
        a: 1.0,
        b: 0.25,
        g: 0.5,
        r: 0.75,
    }
}

const fn shape() -> Rect {
    Rect {
        h: 10.0,
        w: 20.0,
        x: 4.0,
        y: 8.0,
    }
}

fn ramp() -> Paint {
    let stop = |offset| Stop {
        offset,
        color: ink(),
    };
    Paint::Linear {
        from: Pt { x: 0.0, y: 0.0 },
        stops: Stops::new(&[stop(0.0), stop(1.0)])
            .unwrap_or_else(|error| panic!("two stops in order are a ramp: {error}")),
        to: Pt { x: 10.0, y: 0.0 },
    }
}

/// Where a ramp starts and ends, once a list has drawn it.
fn ramp_ends(list: &DrawList) -> (Pt, Pt) {
    match only(list) {
        DrawCmd::Fill {
            paint: Paint::Linear { from, to, .. },
            ..
        } => (*from, *to),
        other => panic!("a ramp was drawn, not {other:?}"),
    }
}

fn only(list: &DrawList) -> &DrawCmd {
    match list.commands() {
        [command] => command,
        other => panic!("one command was drawn, not {}", other.len()),
    }
}

#[kithara::test]
fn a_transformed_run_moves_what_it_draws() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::translate(Pt { x: 5.0, y: -3.0 }), |list| {
        list.fill_rect(shape(), ink());
    });

    assert_eq!(
        only(&list.finish()),
        &DrawCmd::Fill {
            geom: Geom::Rect(Rect {
                h: 10.0,
                w: 20.0,
                x: 9.0,
                y: 5.0,
            }),
            paint: ink().into(),
        }
    );
}

/// An object inside an object is one matrix by the time a point is
/// written down, which is what makes a nested scene exact rather than
/// approximately placed by two toolkits in turn.
#[kithara::test]
fn nested_runs_compose_into_one_transform() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::scale(Pt { x: 2.0, y: 2.0 }), |list| {
        list.transformed(Transform::translate(Pt { x: 1.0, y: 1.0 }), |list| {
            list.fill_rect(shape(), ink());
        });
    });

    assert_eq!(
        only(&list.finish()),
        &DrawCmd::Fill {
            geom: Geom::Rect(Rect {
                h: 20.0,
                w: 40.0,
                x: 10.0,
                y: 18.0,
            }),
            paint: ink().into(),
        }
    );
}

#[kithara::test]
fn the_transform_is_put_back_when_the_run_ends() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::translate(Pt { x: 100.0, y: 100.0 }), |_| {});
    list.fill_rect(shape(), ink());

    assert_eq!(
        only(&list.finish()),
        &DrawCmd::Fill {
            geom: Geom::Rect(shape()),
            paint: ink().into(),
        }
    );
}

/// A turned rectangle is not a rectangle, and the neutral list says so by
/// carrying the outline rather than by handing a backend a rectangle and a
/// matrix it may or may not honour.
#[kithara::test]
fn a_turned_rectangle_becomes_an_outline() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::rotate(0.4), |list| {
        list.fill_rect(shape(), ink());
    });

    assert!(matches!(
        only(&list.finish()),
        DrawCmd::Fill {
            geom: Geom::Path(_),
            ..
        }
    ));
}

#[kithara::test]
fn a_squashed_circle_becomes_an_outline() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::scale(Pt { x: 2.0, y: 1.0 }), |list| {
        list.fill_circle(Pt { x: 0.0, y: 0.0 }, 4.0, ink());
    });

    assert!(matches!(
        only(&list.finish()),
        DrawCmd::Fill {
            geom: Geom::Path(_),
            ..
        }
    ));
}

#[kithara::test]
fn an_evenly_scaled_circle_stays_a_circle() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::scale(Pt { x: 3.0, y: 3.0 }), |list| {
        list.fill_circle(Pt { x: 1.0, y: 2.0 }, 4.0, ink());
    });

    assert_eq!(
        only(&list.finish()),
        &DrawCmd::Fill {
            geom: Geom::Circle {
                center: Pt { x: 3.0, y: 6.0 },
                radius: 12.0,
            },
            paint: ink().into(),
        }
    );
}

#[kithara::test]
fn a_scaled_stroke_scales_its_pen() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::scale(Pt { x: 2.0, y: 2.0 }), |list| {
        list.stroke_line(Pt { x: 0.0, y: 0.0 }, Pt { x: 1.0, y: 0.0 }, ink(), 1.5);
    });
    let drawn = list.finish();

    let DrawCmd::Stroke { pen, .. } = only(&drawn) else {
        panic!("a stroked line is a stroke");
    };
    assert_eq!(pen.width, 3.0);
}

#[kithara::test]
fn a_zero_radius_rounded_fill_is_the_existing_rect_list() {
    let rect = Rect {
        h: 12.0,
        w: 24.0,
        x: 3.0,
        y: 6.0,
    };
    let color = Rgba {
        a: 1.0,
        b: 0.25,
        g: 0.5,
        r: 0.75,
    };
    let mut expected = DrawListBuilder::default();
    expected.fill_rect(rect, color);
    let mut rounded = DrawListBuilder::default();
    rounded.fill_rounded_rect(rect, 0.0, color);

    assert_eq!(rounded.finish(), expected.finish());
}

/// A ramp's geometry is in the same pixels as the shape it fills, which is
/// what lets every backend resolve the same colour at the same place. A
/// transform that moves the shape has to move the ramp with it, or the
/// shape lands one place and its colours another — which is what a Lottie
/// artwork drew before this: a two-colour ramp painted flat, because the
/// ramp stayed at the origin while its rectangle moved 150 across.
#[kithara::test]
fn a_ramp_moves_with_the_shape_it_fills() {
    let mut list = DrawListBuilder::default();
    list.transformed(Transform::translate(Pt { x: 30.0, y: 4.0 }), |list| {
        list.fill_rect(shape(), ramp());
    });

    assert_eq!(
        ramp_ends(&list.finish()),
        (Pt { x: 30.0, y: 4.0 }, Pt { x: 40.0, y: 4.0 })
    );
}

/// The list resolves a transform into the points it draws, so a ramp under
/// no transform is the one the caller wrote.
#[kithara::test]
fn a_ramp_under_no_transform_is_left_alone() {
    let mut list = DrawListBuilder::default();
    list.fill_rect(shape(), ramp());

    assert_eq!(
        ramp_ends(&list.finish()),
        (Pt { x: 0.0, y: 0.0 }, Pt { x: 10.0, y: 0.0 })
    );
}

#[kithara::test]
fn rounded_strokes_retain_native_geometry_and_canonicalize_zero() {
    let rect = Rect {
        h: 12.0,
        w: 24.0,
        x: 3.0,
        y: 6.0,
    };
    let color = Rgba {
        a: 1.0,
        b: 0.25,
        g: 0.5,
        r: 0.75,
    };
    let mut builder = DrawListBuilder::default();
    builder.stroke_rounded_rect(rect, 4.0, color, 1.5);
    builder.stroke_rounded_rect(rect, 0.0, color, 1.5);

    assert_eq!(
        builder.finish().commands(),
        [
            DrawCmd::Stroke {
                geom: Geom::RoundedRect { rect, radius: 4.0 },
                color,
                pen: Pen::new(1.5),
            },
            DrawCmd::Stroke {
                geom: Geom::Rect(rect),
                color,
                pen: Pen::new(1.5),
            },
        ]
    );
}

/// A pen the caller shaped travels to the command untouched.
#[kithara::test]
fn a_shaped_pen_reaches_the_command_it_was_given_to() {
    let color = Rgba {
        a: 1.0,
        b: 0.25,
        g: 0.5,
        r: 0.75,
    };
    let pen = Pen::new(3.0)
        .with_cap(LineCap::Round)
        .with_join(LineJoin::Round);
    let mut builder = DrawListBuilder::default();
    builder.stroke_line(Pt { x: 0.0, y: 0.0 }, Pt { x: 8.0, y: 0.0 }, color, pen);

    assert_eq!(
        builder.finish().commands(),
        [DrawCmd::Stroke {
            geom: Geom::Line {
                from: Pt { x: 0.0, y: 0.0 },
                to: Pt { x: 8.0, y: 0.0 },
            },
            color,
            pen,
        }]
    );
}

#[kithara::test]
fn a_clip_retains_its_region_and_nested_list() {
    let region = Rect {
        h: 20.0,
        w: 40.0,
        x: 3.0,
        y: 6.0,
    };
    let color = Rgba {
        a: 1.0,
        b: 0.25,
        g: 0.5,
        r: 0.75,
    };
    let mut nested = DrawListBuilder::default();
    nested.fill_rect(
        Rect {
            h: 40.0,
            w: 80.0,
            x: -10.0,
            y: -20.0,
        },
        color,
    );
    let nested = nested.finish();
    let mut builder = DrawListBuilder::default();

    builder.clip(region, nested.clone());

    assert_eq!(
        builder.finish().commands(),
        [DrawCmd::Clip {
            region,
            list: nested,
        }]
    );
}
