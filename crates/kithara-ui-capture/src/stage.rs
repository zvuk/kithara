use super::Geometry;

/// One host, turned to a page at a time and photographed.
///
/// The two hosts this toolkit draws with — the immediate one and the retained
/// one — are photographed by the same walk. What differs between them is only
/// what this trait names: how a page is opened, how time is advanced, and what
/// rasterises. Everything around that is the walk in [`super::shoot_set`].
pub trait Stage {
    /// What names a page. The walk writes one file per page per photograph, so
    /// this is also what the file is called.
    type Page;

    /// The physical size and scale every photograph of this stage is taken at.
    /// Fixed for the whole set: a set whose pages differ in geometry cannot be
    /// compared against another set page by page.
    fn geometry(&self) -> Geometry;

    /// Rasterises the open page and answers with its pixels, RGBA8, row major,
    /// borrowed from whatever storage the stage rasterised into. The stage owns
    /// them because only the stage knows how they are produced: one host reads
    /// them back from a texture it keeps, the other is handed them by its
    /// renderer.
    ///
    /// # Errors
    /// Fails when no page is open, or when the page cannot be drawn or read
    /// back.
    fn shoot(&mut self) -> Result<&[u8], String>;

    /// Advances the open page by one step of this stage's own clock.
    fn tick(&mut self);

    /// Opens a page, ready to be photographed as it stands.
    ///
    /// # Errors
    /// Fails when the page cannot be opened, which for a retained host means
    /// its document did not compile or did not mount.
    fn turn(&mut self, page: &Self::Page) -> Result<(), String>;
}
