use std::sync::LazyLock;

use num_traits::cast::AsPrimitive;
use velato::Composition;

/// The artwork the toolkit ships, named the way a document names a sprite
/// sheet.
const PULSE: &str = "pulse";

/// One artwork, read once.
///
/// Reading a Lottie is parsing a document, so it is done at most once per name
/// for the life of the process and every drawing borrows the result. What a
/// frame costs is the emitting, which is per frame by nature.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct Artwork {
    #[field(get(vis = "pub(crate)"))]
    composition: Composition,
}

impl Artwork {
    /// The box the artwork was authored in, which is what a drawing fits into
    /// its own.
    pub(crate) fn size(&self) -> (f64, f64) {
        let width: f64 = self.composition.width.max(1).as_();
        let height: f64 = self.composition.height.max(1).as_();
        (width, height)
    }

    /// Which frame of this artwork stands `seconds` into a pass of `pass`.
    ///
    /// Wraps, so a clock that keeps running keeps playing. A pass of nothing at
    /// all holds the first frame rather than dividing by it.
    pub(crate) fn frame_at(&self, seconds: f32, pass: f32) -> f64 {
        let frames = &self.composition.frames;
        let span = frames.end - frames.start;
        if !pass.is_finite() || pass <= 0.0 || !seconds.is_finite() || span <= 0.0 {
            return frames.start;
        }
        let through = f64::from(seconds / pass).rem_euclid(1.0);

        span.mul_add(through, frames.start)
    }
}

/// The artwork of that name, or nothing.
///
/// An artwork the toolkit does not ship draws nothing, which is what an unbound
/// control does everywhere else.
#[must_use]
pub fn builtin_artwork(name: &str) -> Option<&'static Artwork> {
    static ARTWORK: LazyLock<Option<Artwork>> = LazyLock::new(|| {
        Composition::from_slice(include_str!("../../assets/lottie/pulse.json").as_bytes())
            .inspect_err(|error| tracing::error!(%error, "the built-in artwork did not read"))
            .ok()
            .map(|composition| Artwork { composition })
    });
    (name == PULSE).then(|| ARTWORK.as_ref()).flatten()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{PULSE, builtin_artwork};

    fn shipped() -> &'static super::Artwork {
        builtin_artwork(PULSE).unwrap_or_else(|| panic!("the toolkit ships one artwork"))
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
}
