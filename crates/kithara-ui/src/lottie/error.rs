/// Why an artwork could not be drawn.
///
/// Every one of these is a refusal of the whole picture rather than of one
/// shape in it: an artwork that half draws is a picture nobody authored, and
/// the neutral list already refuses a whole list a backend cannot draw.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum LottieError {
    /// A shape that changes contours already collected rather than adding one.
    ///
    /// Velato reads the trim of these and drops the repeater on the floor, so
    /// what reaches this today is always a trim; both are named together
    /// because the emitter applies neither.
    #[error("layer {layer:?} modifies its contours after the fact, which this emitter does not do")]
    Modifier { layer: String },
    #[error("layer {layer:?} instances precomposition {name:?}, which the emitter does not enter")]
    Instance { layer: String, name: String },
    #[error("layer {layer:?} carries a mask, which the neutral list clips only by a rectangle")]
    Mask { layer: String },
    #[error("layer {layer:?} is matted by another, which is a blend the list has no word for")]
    Matte { layer: String },
    /// A brush this list has no word for: a picture, a ramp swept around a
    /// point, or one running between two circles.
    ///
    /// Velato builds none of the three — it makes a solid, a linear ramp or a
    /// ramp out from one centre, and nothing else — so this names the shape of
    /// the type it reads rather than something an artwork can ask for today.
    #[error("layer {layer:?} paints with a brush this list has no word for")]
    Brush { layer: String },
    #[error("layer {layer:?} ramps a stroke, and a stroke in this list carries one colour")]
    RampedStroke { layer: String },
    #[error("layer {layer:?} has a ramp this list refuses: {source}")]
    Ramp {
        layer: String,
        source: crate::draw::StopsError,
    },
    #[error("layer {layer:?} limits its miter, which this list's pen leaves to the backend")]
    MiteredStroke { layer: String },
    #[error("layer {layer:?} blends with what is under it, which this list draws over instead")]
    Blend { layer: String },
}
