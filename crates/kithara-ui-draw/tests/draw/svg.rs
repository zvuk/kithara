use kithara_test_utils::kithara;
use kithara_ui_draw::{FillRule, SvgError, Verb, outline};

fn document(body: &str) -> String {
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">{body}</svg>"#)
}

/// A square drawn corner to corner in its own coordinates lands corner to
/// corner in the unit square.
#[kithara::test]
fn a_view_box_becomes_the_unit_square() {
    let read = outline(&document(r#"<path d="M0,0 L10,0 L10,10 Z"/>"#))
        .unwrap_or_else(|error| panic!("a one-path document reads: {error}"));

    assert_eq!(
        read.placed(kithara_ui_draw::Rect {
            h: 1.0,
            w: 1.0,
            x: 0.0,
            y: 0.0,
        })
        .verbs(),
        [
            Verb::MoveTo(kithara_ui_draw::Pt { x: 0.0, y: 0.0 }),
            Verb::LineTo(kithara_ui_draw::Pt { x: 1.0, y: 0.0 }),
            Verb::LineTo(kithara_ui_draw::Pt { x: 1.0, y: 1.0 }),
            Verb::Close,
        ]
    );
}

/// A view box taller than it is wide keeps its proportions and sits in the
/// middle, which is what SVG itself does with one.
#[kithara::test]
fn a_lopsided_view_box_keeps_its_proportions() {
    let read = outline(r#"<svg viewBox="0 0 5 10"><path d="M0,0 L5,10"/></svg>"#)
        .unwrap_or_else(|error| panic!("a one-path document reads: {error}"));
    let placed = read.placed(kithara_ui_draw::Rect {
        h: 1.0,
        w: 1.0,
        x: 0.0,
        y: 0.0,
    });

    assert_eq!(
        placed.verbs(),
        [
            Verb::MoveTo(kithara_ui_draw::Pt { x: 0.25, y: 0.0 }),
            Verb::LineTo(kithara_ui_draw::Pt { x: 0.75, y: 1.0 }),
        ]
    );
}

#[kithara::test]
fn the_fill_rule_travels_with_the_outline() {
    let even_odd = outline(&document(
        r#"<path fill-rule="evenodd" d="M0,0 L10,10 Z"/>"#,
    ))
    .unwrap_or_else(|error| panic!("an even-odd document reads: {error}"));
    let plain = outline(&document(r#"<path d="M0,0 L10,10 Z"/>"#))
        .unwrap_or_else(|error| panic!("a plain document reads: {error}"));

    assert_eq!(
        even_odd
            .placed(kithara_ui_draw::Rect {
                h: 1.0,
                w: 1.0,
                x: 0.0,
                y: 0.0
            })
            .rule(),
        FillRule::EvenOdd
    );
    assert_eq!(
        plain
            .placed(kithara_ui_draw::Rect {
                h: 1.0,
                w: 1.0,
                x: 0.0,
                y: 0.0
            })
            .rule(),
        FillRule::NonZero
    );
}

/// What this cannot draw it refuses, rather than reading the part it
/// understands and leaving the rest off the screen.
#[kithara::test]
fn a_document_this_cannot_read_says_so() {
    assert_eq!(
        outline("<svg viewBox=\"0 0 1 1\"><circle r=\"1\"/></svg>"),
        Err(SvgError::NotAPath("circle".to_owned()))
    );
    assert_eq!(
        outline(r#"<svg><path d="M0,0"/></svg>"#),
        Err(SvgError::NoViewBox)
    );
    assert_eq!(
        outline(r#"<svg viewBox="0 0 0 10"><path d="M0,0"/></svg>"#),
        Err(SvgError::ViewBox("0 0 0 10".to_owned()))
    );
    assert_eq!(
        outline(r#"<svg viewBox="0 0 10 10"></svg>"#),
        Err(SvgError::Empty)
    );
    assert_eq!(
        outline(&document(r#"<path fill-rule="inherit" d="M0,0"/>"#)),
        Err(SvgError::Rule("inherit".to_owned()))
    );
    assert_eq!(
        outline(&document(
            r#"<path d="M0,0"/><path fill-rule="evenodd" d="M1,1"/>"#
        )),
        Err(SvgError::MixedRules)
    );
    assert!(matches!(
        outline(&document(r#"<path d="Q"/>"#)),
        Err(SvgError::Data(_))
    ));
    assert!(matches!(outline("<svg"), Err(SvgError::Malformed(_))));
    assert_eq!(
        outline("<html><body/></html>"),
        Err(SvgError::NotSvg("html".to_owned()))
    );
}
