use std::fmt::Display;

/// Which pages a capture photographs, how many photographs of each, and how
/// many of the host's own frames run between two photographs.
///
/// A page whose document moves draws a different picture at every photograph,
/// which a single still cannot show.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Film<Page> {
    pub pages: Vec<Page>,
    pub photos: usize,
    pub steps: usize,
}

impl<Page> Film<Page> {
    /// Several photographs of each page, with the host's own clock running
    /// between them.
    ///
    /// # Errors
    /// Refuses a film of no photographs, and one that leaves no time between
    /// two photographs: both would photograph one picture twice and call it
    /// motion.
    pub fn new(pages: Vec<Page>, photos: usize, steps: usize) -> Result<Self, String> {
        if photos == 0 {
            return Err("a film takes at least one photograph of a page".to_owned());
        }
        if photos > 1 && steps == 0 {
            return Err("a film of several photographs needs time between them".to_owned());
        }
        Ok(Self {
            pages,
            photos,
            steps,
        })
    }

    /// Every page once, which is what a capture with no film asked for takes.
    #[must_use]
    pub fn stills(pages: Vec<Page>) -> Self {
        Self {
            pages,
            photos: 1,
            steps: 0,
        }
    }
}

impl<Page: Display> Film<Page> {
    /// Where one photograph of a page lands.
    pub fn file(&self, page: &Page, photo: usize) -> String {
        page_file(page, (self.photos > 1).then_some(photo))
    }
}

/// Where a photograph of a page lands: the page's own name, and the number of
/// the photograph when a film takes more than one of it.
///
/// A page photographed once keeps the page's own name, so a still set and a
/// film of one are the same set of files.
pub fn page_file<Page: Display>(page: &Page, photo: Option<usize>) -> String {
    photo.map_or_else(
        || format!("{page}.png"),
        |index| format!("{page}-{index:03}.png"),
    )
}
