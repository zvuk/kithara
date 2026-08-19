use bon::Builder;
#[cfg(feature = "render")]
use num_traits::cast::AsPrimitive;

use crate::{ids::InternId, mount::Control, size::SizeSpec, skin::SkinDoc};

/// One frame of a named sheet, played by whatever answers its endpoint.
///
/// The endpoint hands over seconds, so a document that binds it to the host's
/// own clock gets an animation without the application owning a timer; one that
/// binds it to something else scrubs the sheet by hand from the same field.
#[derive(Builder)]
pub(crate) struct Sprite {
    pub(crate) sheet: InternId,
    /// How long one pass through every frame of the sheet takes.
    pub(crate) seconds: f32,
}

impl Control for Sprite {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.vis.size
    }
}

/// Which frame a sheet of `frames` shows `seconds` into a pass of `pass`.
///
/// Wraps, so a clock that keeps running keeps playing. A pass of nothing at all
/// holds the first frame rather than dividing by it.
///
/// Only where there is drawing: a build that mounts documents without painting
/// them never asks which frame shows.
#[cfg(feature = "render")]
pub(crate) fn frame_at(seconds: f32, pass: f32, frames: usize) -> usize {
    if frames == 0 || !pass.is_finite() || pass <= 0.0 || !seconds.is_finite() {
        return 0;
    }
    let through = (seconds / pass).rem_euclid(1.0);
    let count: f32 = frames.as_();
    let index: usize = (through * count).as_();
    index.min(frames - 1)
}

#[cfg(feature = "render")]
mod host {
    use num_traits::cast::AsPrimitive;

    use super::{Sprite, frame_at};
    #[cfg(feature = "masonry")]
    use crate::render::controls::DataRefresh;
    use crate::{
        atoms::picture::sprite::Sprite as Face,
        draw::Image,
        render::{
            ReadValue, Skin, builtin_sheet,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Sprite {
        type Painter = Face;

        fn painter(&self, _skin: &Skin) -> Face {
            Face
        }

        fn data(&self, read: Reading<'_>) -> Option<Option<Image>> {
            Some(frame(
                read.ctx.ui.resolve(self.sheet),
                self.seconds,
                seconds(read.value),
            ))
        }

        /// A retained host mounts a leaf once and then only hears about it
        /// again if something says the leaf changed, so the sheet is stepped by
        /// asking its endpoint afresh rather than by the mount that built it.
        #[cfg(feature = "masonry")]
        fn retained_refresh(
            &self,
            read: Reading<'_>,
            endpoint: Option<&str>,
        ) -> Option<DataRefresh<Option<Image>>> {
            let sheet = read.ctx.ui.resolve(self.sheet).to_owned();
            let endpoint = endpoint?.to_owned();
            let pass = self.seconds;
            Some(Box::new(move |data, ctx| {
                let next = frame(&sheet, pass, seconds(ctx.get(&endpoint).as_ref()));
                if next.as_ref().map(Image::id) == data.as_ref().map(Image::id) {
                    return false;
                }
                *data = next;
                true
            }))
        }
    }

    /// How far the sheet has run, as its endpoint says.
    fn seconds(value: Option<&ReadValue<'_>>) -> f32 {
        match value {
            Some(ReadValue::Scalar(seconds)) => seconds.as_(),
            _ => 0.0,
        }
    }

    fn frame(sheet: &str, pass: f32, seconds: f32) -> Option<Image> {
        let sheet = builtin_sheet(sheet)?;
        sheet.frame(frame_at(seconds, pass, sheet.len())).cloned()
    }

    /// What a reading turns into, measured through the mapping the control
    /// itself uses rather than through the arithmetic under it.
    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::frame;
        use crate::draw::{Image, ImageId};

        /// The pass a document would give a spinner: long enough to read.
        const PASS: f32 = 1.6;

        fn at(seconds: f32) -> Option<ImageId> {
            frame("spinner", PASS, seconds).map(|image| image.id().clone())
        }

        #[kithara::test]
        fn a_reading_partway_through_a_pass_picks_a_later_frame() {
            assert_ne!(at(0.0), at(PASS / 2.0));
        }

        /// The whole point of a sheet: a clock that keeps running keeps
        /// playing, rather than stopping on the last frame it reached.
        #[kithara::test]
        fn a_reading_a_whole_pass_later_comes_back_to_the_same_frame() {
            assert_eq!(at(0.0), at(PASS));
        }

        #[kithara::test]
        fn a_sheet_the_toolkit_does_not_ship_draws_nothing() {
            assert_eq!(frame("no-such-sheet", PASS, 0.0), None);
        }

        /// The picture carries its pixels, so a rasteriser is handed everything
        /// it needs without a registry reaching across the seam behind it.
        #[kithara::test]
        fn the_frame_a_reading_picks_carries_its_pixels() {
            assert!(
                frame("spinner", PASS, 0.0)
                    .as_ref()
                    .and_then(Image::rgba)
                    .is_some()
            );
        }
    }
}

#[cfg(all(test, feature = "render"))]
mod tests {
    use kithara_test_utils::kithara;

    use super::frame_at;

    #[kithara::test]
    fn the_start_of_a_pass_shows_the_first_frame() {
        assert_eq!(frame_at(0.0, 2.0, 8), 0);
    }

    #[kithara::test]
    fn halfway_through_a_pass_shows_the_middle_frame() {
        assert_eq!(frame_at(1.0, 2.0, 8), 4);
    }

    #[kithara::test]
    fn a_whole_pass_later_shows_the_first_frame_again() {
        assert_eq!(frame_at(2.0, 2.0, 8), 0);
    }

    #[kithara::test]
    fn the_last_frame_is_reached_before_the_pass_ends() {
        assert_eq!(frame_at(1.99, 2.0, 8), 7);
    }

    /// A pass of no length is a document defect, and the answer to it is the
    /// first frame rather than a division by nothing.
    #[kithara::test]
    fn a_pass_of_no_length_holds_the_first_frame() {
        assert_eq!(frame_at(3.0, 0.0, 8), 0);
    }

    #[kithara::test]
    fn a_sheet_with_no_frames_asks_for_the_first_one() {
        assert_eq!(frame_at(3.0, 2.0, 0), 0);
    }
}
