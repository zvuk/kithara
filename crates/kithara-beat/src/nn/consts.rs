/// Chunk geometry, frame rate, and mel bins the trained model fixes.
pub(super) struct Consts;

impl Consts {
    /// Frames discarded from each edge of predictions.
    pub(super) const BORDER_SIZE: i64 = 6;
    /// Frames per chunk (30 seconds at 50 fps).
    pub(super) const CHUNK_SIZE: i64 = 1500;
    pub(super) const FPS: f32 = 50.0;
    /// Mel bins the model emits per frame.
    pub(super) const MEL_BINS: usize = 128;
    /// Effective step between chunks.
    pub(super) const STRIDE: i64 = Self::CHUNK_SIZE - 2 * Self::BORDER_SIZE;
}
