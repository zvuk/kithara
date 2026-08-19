//! The neutral Lottie emitter against the renderer it was written from.
//!
//! The pairing rule an artwork's draws follow lives in a private structure
//! inside velato, so no public call can be asked whether this reimplementation
//! of it agrees. Pixels can: the same artwork at the same frame is painted
//! twice into the same rasteriser, once by velato itself and once by the
//! neutral list, and the two pictures are compared.

use std::fmt::{self, Display, Formatter};

use kithara_test_utils::kithara;
use velato::{Composition, Renderer as LottieRenderer};
use vello::{Scene, kurbo::Affine, peniko::color::palette};

use super::{VelloBackend, conformance::rasterise_at};
use crate::{
    draw::{DrawListBuilder, replay},
    lottie::emit::emit,
};

/// One artwork with a merged pair of contours under one fill, a stroked
/// open contour, a ramp, a turned layer and a layer opacity — one of each
/// thing the pairing rule and the alpha fold decide.
const PROBE: &str = include_str!("../../assets/lottie/probe.json");

fn artwork() -> Composition {
    Composition::from_slice(PROBE.as_bytes())
        .unwrap_or_else(|error| panic!("the probe artwork must read: {error}"))
}

fn painted(scene: &Scene) -> Vec<u8> {
    let side = u32::try_from(Apart::CANVAS).unwrap_or(u32::MAX);
    rasterise_at(scene, (side, side), palette::css::BLACK)
        .unwrap_or_else(|error| panic!("vello must rasterise: {error}"))
}

/// What velato paints, which is the answer this emitter is measured against.
fn oracle(frame: f64) -> Vec<u8> {
    let mut scene = Scene::new();
    LottieRenderer::new().append(&artwork(), frame, Affine::IDENTITY, 1.0, &mut scene);
    painted(&scene)
}

/// What the neutral list paints, through the backend the application uses.
fn seam(frame: f64) -> Vec<u8> {
    let mut list = DrawListBuilder::default();
    emit(&artwork(), frame, &mut list)
        .unwrap_or_else(|error| panic!("the probe artwork must draw: {error}"));
    let mut scene = Scene::new();
    replay(&list.finish(), &mut VelloBackend::new(&mut scene));
    painted(&scene)
}

/// What the two pictures disagree about.
///
/// The two paths reach one rasteriser through different arithmetic — `f64`
/// and a transform stream through velato, `f32` and baked points through this
/// list — so a wrong pairing rule and one antialiased edge rounded to the
/// other side of a byte read alike from a count alone. What tells them apart
/// is how far a pixel is off and whether it sits in flat paint, and both are
/// carried here so a failure on a machine this branch cannot rasterise on is
/// still evidence.
struct Apart {
    pixels: usize,
    /// The largest single-channel difference anywhere.
    worst: u8,
    /// How many of them are inside flat paint rather than on an edge.
    flat: usize,
    first: Option<(usize, [u8; 4], [u8; 4])>,
}

impl Display for Apart {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let Some((at, oracle, seam)) = self.first else {
            return write!(formatter, "the two pictures are the same");
        };
        write!(
            formatter,
            "{} of {} pixels apart, {} of them in flat paint, worst {} of 255, first at ({}, {}): velato {oracle:?} against the list {seam:?}",
            self.pixels,
            Self::CANVAS * Self::CANVAS,
            self.flat,
            self.worst,
            at % Self::CANVAS,
            at / Self::CANVAS,
        )
    }
}

impl Apart {
    /// The artwork's own canvas, so nothing is scaled on the way to a pixel.
    const CANVAS: usize = 200;

    fn at(frame: f64) -> Self {
        let (oracle, seam) = (oracle(frame), seam(frame));
        assert_eq!(oracle.len(), seam.len(), "the two pictures are one size");

        let mut apart = Self {
            pixels: 0,
            worst: 0,
            flat: 0,
            first: None,
        };
        for (at, (left, right)) in oracle.chunks(4).zip(seam.chunks(4)).enumerate() {
            if left == right {
                continue;
            }
            apart.pixels += 1;
            let worst = left
                .iter()
                .zip(right.iter())
                .map(|(one, two)| one.abs_diff(*two))
                .max()
                .unwrap_or(0);
            apart.worst = apart.worst.max(worst);
            if Self::flat_at(&oracle, at) {
                apart.flat += 1;
            }
            if apart.first.is_none() {
                let pair = |bytes: &[u8]| [bytes[0], bytes[1], bytes[2], bytes[3]];
                apart.first = Some((at, pair(left), pair(right)));
            }
        }
        apart
    }

    /// Whether a pixel sits in flat paint: every neighbour it has is the very
    /// colour it is. Nothing there is part-covered, so its colour is a fill's
    /// own and an antialiased edge cannot be what moved it.
    fn flat_at(picture: &[u8], at: usize) -> bool {
        let (x, y) = (at % Self::CANVAS, at / Self::CANVAS);
        if x == 0 || y == 0 || x + 1 == Self::CANVAS || y + 1 == Self::CANVAS {
            return false;
        }
        let pixel = |at: usize| picture.get(at * 4..at * 4 + 4);
        let here = pixel(at);
        [at - 1, at + 1, at - Self::CANVAS, at + Self::CANVAS]
            .into_iter()
            .all(|neighbour| pixel(neighbour) == here)
    }
}

/// A fill is a fill: where the artwork is one flat colour, nothing is
/// part-covered, so both paths write the fill's own bytes and the rounding an
/// edge is allowed cannot reach. This is where a mispaired contour, a layer
/// alpha folded into the wrong place or a ramp read the wrong way round would
/// show, and it is asserted with no slack at all.
#[kithara::test]
fn the_flat_paint_of_the_artwork_is_the_same_colour_in_both() {
    let apart = Apart::at(0.0);

    assert_eq!(apart.flat, 0, "{apart}");
}

/// The other half of the same picture: the edges. Coverage comes back
/// quantised to one part in 255, and two coverages that differ by less than
/// half of that still land either side of a rounding boundary — on one
/// adapter and not on another. Asking for identical bytes asks the instrument
/// for precision it does not have; this asks for all of the precision it
/// does, and a wrong rule is worth many steps rather than one.
#[kithara::test]
fn no_pixel_of_the_artwork_is_off_by_more_than_the_target_can_hold() {
    let apart = Apart::at(0.0);

    assert!(apart.worst <= 1, "{apart}");
}

/// Halfway through, where a turned layer's own matrix is what the two have to
/// agree about rather than the shapes alone.
#[kithara::test]
fn the_flat_paint_stays_the_same_colour_partway_through() {
    let apart = Apart::at(30.0);

    assert_eq!(apart.flat, 0, "{apart}");
}

#[kithara::test]
fn no_pixel_is_off_by_more_than_the_target_can_hold_partway_through() {
    let apart = Apart::at(30.0);

    assert!(apart.worst <= 1, "{apart}");
}
