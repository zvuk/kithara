use bon::Builder;

use crate::{expand::Binding, ids::InternId};

/// One frame of a named artwork, played by whatever answers its endpoint.
///
/// The endpoint hands over seconds, so a document that binds it to the host's
/// own clock gets an animation without the application owning a timer; one that
/// binds it to something else scrubs the artwork by hand from the same field.
/// This is the sheet contract with a drawing in place of a picture.
#[derive(Builder, kithara_derive::ViewControl, kithara_derive::Control)]
#[control(size = skin.vis.size)]
#[derive(kithara_derive::NodeControl)]
pub(crate) struct Lottie<'a> {
    pub(crate) artwork: InternId,
    /// The flag that says which of the two artworks stands. It is an endpoint
    /// of its own rather than the control's value, which carries seconds.
    pub(crate) active: Option<&'a Binding>,
    /// The artwork shown while `active` reads true.
    pub(crate) active_artwork: Option<InternId>,
    /// How long one pass through the whole artwork takes.
    pub(crate) seconds: f32,
}

#[cfg(feature = "render")]
mod host {
    use num_traits::cast::AsPrimitive;

    use super::Lottie;
    #[cfg(feature = "masonry")]
    use crate::render::controls::DataRefresh;
    use crate::{
        atoms::picture::lottie::{Lottie as Face, Standing},
        expand::Binding,
        lottie::builtin_artwork,
        render::{
            Ctx, ReadValue, Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Lottie<'_> {
        type Painter = Face;

        fn data(&self, read: Reading<'_>) -> Option<Standing> {
            let name = self
                .active_artwork
                .filter(|_| active(self.active, read.ctx))
                .unwrap_or(self.artwork);

            Some(standing(
                read.ctx.ui.resolve(name),
                self.seconds,
                seconds(read.value),
            ))
        }

        fn painter(&self, _skin: &Skin) -> Face {
            Face
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
            let active_artwork = self
                .active_artwork
                .map(|name| read.ctx.ui.resolve(name).to_owned());
            let flag = read.ctx.endpoint(self.active).map(ToOwned::to_owned);
            let endpoint = endpoint?.to_owned();
            let pass = self.seconds;
            Some(Box::new(move |data, ctx| {
                let showing = active_artwork
                    .as_deref()
                    .filter(|_| flag.as_deref().is_some_and(|flag| flagged(ctx.get(flag))))
                    .unwrap_or(&artwork);
                let next = standing(showing, pass, seconds(ctx.get(&endpoint).as_ref()));
                if next == *data {
                    return false;
                }
                *data = next;
                true
            }))
        }
    }

    /// Which artwork stands, as the flag's own endpoint says.
    fn active(binding: Option<&Binding>, ctx: Ctx<'_, '_>) -> bool {
        binding.is_some_and(|binding| flagged(ctx.read(binding)))
    }

    fn flagged(value: Option<ReadValue<'_>>) -> bool {
        matches!(value, Some(ReadValue::Bool(true)))
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

        mod consts {
            /// The pass a document would give the shipped artwork: long enough to
            /// read.
            pub(super) const PASS: f32 = 2.0;
        }

        fn frame(name: &str, pass: f32, seconds: f32) -> f64 {
            standing(name, pass, seconds).frame
        }

        #[kithara::test]
        fn a_reading_partway_through_a_pass_stands_at_a_later_frame() {
            assert!(
                frame("pulse", consts::PASS, consts::PASS / 2.0)
                    > frame("pulse", consts::PASS, 0.0)
            );
        }

        /// The whole point of a pass: a clock that keeps running keeps playing,
        /// rather than stopping on the last frame it reached.
        #[kithara::test]
        fn a_reading_a_whole_pass_later_comes_back_to_the_same_frame() {
            assert_eq!(
                frame("pulse", consts::PASS, consts::PASS),
                frame("pulse", consts::PASS, 0.0)
            );
        }

        /// An artwork the toolkit does not ship draws nothing, which is what an
        /// unbound control does everywhere else.
        #[kithara::test]
        fn an_artwork_the_toolkit_does_not_ship_stands_at_no_frame() {
            assert_eq!(frame("nothing-of-the-sort", consts::PASS, 1.0), 0.0);
        }
    }
}
