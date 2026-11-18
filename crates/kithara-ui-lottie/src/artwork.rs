use std::sync::LazyLock;

use kithara_platform::sync::Arc;
use kithara_ui_draw::DrawListBuilder;
use num_traits::cast::AsPrimitive;
use velato::Composition;

use crate::{ArtworkError, emit, validate};

/// One artwork, read once.
///
/// A skin or application catalog parses its Lottie document once and shares
/// the immutable result between drawings. Emitting the drawing costs work per frame.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct Artwork {
    #[field(get)]
    composition: Composition,
}

impl Artwork {
    /// Parses one owned artwork for a skin or application resource catalog.
    ///
    /// # Errors
    /// Returns a typed error for malformed, excessive or unsupported artwork.
    pub fn parse(bytes: &[u8]) -> Result<Self, ArtworkError> {
        let artwork = Self {
            composition: validate::parse(bytes)?,
        };
        // Numeric smoke checks supplement whole-document structural admission.
        for phase in [0.0, 0.5, 0.999] {
            emit(
                artwork.composition(),
                artwork.frame_at(phase, 1.0),
                &mut DrawListBuilder::default(),
            )?;
        }
        Ok(artwork)
    }
    /// Which frame of this artwork stands `seconds` into a pass of `pass`.
    ///
    /// Wraps, so a clock that keeps running keeps playing. A pass of nothing at
    /// all holds the first frame rather than dividing by it.
    #[must_use]
    pub fn frame_at(&self, seconds: f32, pass: f32) -> f64 {
        let frames = &self.composition.frames;
        let span = frames.end - frames.start;
        if !pass.is_finite() || pass <= 0.0 || !seconds.is_finite() || span <= 0.0 {
            return frames.start;
        }
        let through = (f64::from(seconds) / f64::from(pass)).rem_euclid(1.0);

        span.mul_add(through, frames.start)
    }

    /// The box the artwork was authored in, which is what a drawing fits into
    /// its own.
    #[must_use]
    pub fn size(&self) -> (f64, f64) {
        let width: f64 = self.composition.width.max(1).as_();
        let height: f64 = self.composition.height.max(1).as_();
        (width, height)
    }
}

/// The artwork of that name, or nothing.
///
/// An artwork the toolkit does not ship draws nothing, which is what an unbound
/// control does everywhere else.
#[must_use]
pub fn builtin_artwork(name: &str) -> Option<&'static Arc<Artwork>> {
    /// The artwork the toolkit ships, named the way a document names a sprite
    /// sheet.
    const PULSE: &str = "pulse";
    /// The second artwork, so a document that switches between two of them has
    /// two to switch between.
    const SPARK: &str = "spark";

    static PULSE_ARTWORK: LazyLock<Option<Arc<Artwork>>> =
        LazyLock::new(|| read(include_str!("../assets/lottie/pulse.json")));
    static SPARK_ARTWORK: LazyLock<Option<Arc<Artwork>>> =
        LazyLock::new(|| read(include_str!("../assets/lottie/spark.json")));

    match name {
        PULSE => PULSE_ARTWORK.as_ref(),
        SPARK => SPARK_ARTWORK.as_ref(),
        _ => None,
    }
}

fn read(text: &str) -> Option<Arc<Artwork>> {
    Artwork::parse(text.as_bytes())
        .inspect_err(|error| tracing::error!(%error, "the built-in artwork did not read"))
        .ok()
        .map(Arc::new)
}
