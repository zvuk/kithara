use bon::Builder;

use crate::{ids::InternId, mount::Control, size::SizeSpec, skin::SkinDoc};

/// One frame of a named artwork, played by whatever answers its endpoint.
///
/// The endpoint hands over seconds, so a document that binds it to the host's
/// own clock gets an animation without the application owning a timer; one that
/// binds it to something else scrubs the artwork by hand from the same field.
/// This is the sheet contract with a drawing in place of a picture.
#[derive(Builder)]
pub(crate) struct Lottie {
    pub(crate) artwork: InternId,
    /// How long one pass through the whole artwork takes.
    pub(crate) seconds: f32,
}

impl Control for Lottie {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.vis.size
    }
}

#[cfg(feature = "render")]
mod host {
    use num_traits::cast::AsPrimitive;

    use super::Lottie;
    #[cfg(feature = "masonry")]
    use crate::render::controls::DataRefresh;
    use crate::{
        atoms::picture::lottie::{Lottie as Face, Standing},
        lottie::builtin_artwork,
        render::{
            ReadValue, Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Lottie {
        type Painter = Face;

        fn painter(&self, _skin: &Skin) -> Face {
            Face
        }

        fn data(&self, read: Reading<'_>) -> Option<Standing> {
            Some(standing(
                read.ctx.ui.resolve(self.artwork),
                self.seconds,
                seconds(read.value),
            ))
        }

        /// A retained host mounts a leaf once and then only hears about it
        /// again if something says the leaf changed, so the artwork is stepped
        /// by asking its endpoint afresh rather than by the mount that built it.
        #[cfg(feature = "masonry")]
        fn retained_refresh(
            &self,
            read: Reading<'_>,
            endpoint: Option<&str>,
        ) -> Option<DataRefresh<Standing>> {
            let artwork = read.ctx.ui.resolve(self.artwork).to_owned();
            let endpoint = endpoint?.to_owned();
            let pass = self.seconds;
            Some(Box::new(move |data, ctx| {
                let next = standing(&artwork, pass, seconds(ctx.get(&endpoint).as_ref()));
                if (next.frame - data.frame).abs() < f64::EPSILON {
                    return false;
                }
                *data = next;
                true
            }))
        }
    }

    /// How far the artwork has run, as its endpoint says.
    fn seconds(value: Option<&ReadValue<'_>>) -> f32 {
        match value {
            Some(ReadValue::Scalar(seconds)) => seconds.as_(),
            _ => 0.0,
        }
    }

    fn standing(name: &str, pass: f32, seconds: f32) -> Standing {
        let artwork = builtin_artwork(name);

        Standing {
            frame: artwork.map_or(0.0, |artwork| artwork.frame_at(seconds, pass)),
            artwork,
        }
    }

    /// What a reading turns into, measured through the mapping the control
    /// itself uses rather than through the arithmetic under it.
    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::standing;

        fn frame(name: &str, pass: f32, seconds: f32) -> f64 {
            standing(name, pass, seconds).frame
        }

        /// The pass a document would give the shipped artwork: long enough to
        /// read.
        const PASS: f32 = 2.0;

        #[kithara::test]
        fn a_reading_partway_through_a_pass_stands_at_a_later_frame() {
            assert!(frame("pulse", PASS, PASS / 2.0) > frame("pulse", PASS, 0.0));
        }

        /// The whole point of a pass: a clock that keeps running keeps playing,
        /// rather than stopping on the last frame it reached.
        #[kithara::test]
        fn a_reading_a_whole_pass_later_comes_back_to_the_same_frame() {
            assert_eq!(frame("pulse", PASS, PASS), frame("pulse", PASS, 0.0));
        }

        /// An artwork the toolkit does not ship draws nothing, which is what an
        /// unbound control does everywhere else.
        #[kithara::test]
        fn an_artwork_the_toolkit_does_not_ship_stands_at_no_frame() {
            assert_eq!(frame("nothing-of-the-sort", PASS, 1.0), 0.0);
        }
    }
}
