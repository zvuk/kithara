use kithara_ui::{
    error::UiDocError,
    ids::SourceUri,
    module::IconName,
    render::{ReadValue, TableRow, WriteValue},
    text::TextDoc,
};

/// Library page collection and origin for source caption errors.
pub const PAGES: &str = "app-library/pages";

/// One branch of the library tree and the page its nodes show.
pub trait LibrarySource {
    /// A read its page declares, by the endpoint's name under `source.`.
    fn read(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }

    /// A write its page declares, by the endpoint's name under `source.`,
    /// delivered to this source alone.
    fn write(&mut self, _endpoint: &str, _value: &WriteValue) {}

    /// The playable source of a listed row in the queue's canonical form,
    /// which the shell keys analysis by.
    fn analysis_key(&self, row: usize) -> Option<&str>;

    fn branch(&self) -> &BranchNode;

    fn expand(&mut self, node: &str);

    fn id(&self) -> &str;

    fn rows(&self, selected: Option<&str>) -> Vec<TableRow<'_>>;

    fn row_key(&self, row: usize) -> Option<&str>;

    fn select(&mut self, node: &str);

    fn status(&self) -> PageStatus;

    fn tick(&mut self);
}

/// Where a source's page stands; the shell words it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageStatus {
    /// It lists at least one row.
    Ready,
    Loading,
    /// It has no row to list.
    Empty,
    Unreadable,
}

/// One node of a source's branch.
pub struct BranchNode {
    /// Names the node to its source; unique within the branch.
    pub key: String,
    pub label: String,
    pub icon: IconName,
    pub count: Option<u32>,
    pub children: Vec<Self>,
    /// Its children are not known yet; it opens all the same.
    pub unlisted: bool,
    /// Selecting it shows a page of its own. A node with children and no page
    /// opens and closes on a press of its whole row.
    pub page: bool,
}

impl BranchNode {
    #[must_use]
    pub fn new(key: &str, label: String, icon: IconName) -> Self {
        Self {
            label,
            icon,
            key: key.to_owned(),
            count: None,
            children: Vec::new(),
            unlisted: false,
            page: false,
        }
    }
}

/// The catalog's words for a source's fixed label.
///
/// # Errors
/// Returns [`UiDocError::UnknownTextKey`] when the catalog does not word `key`.
pub fn worded(text: &TextDoc, key: &str, path: &str) -> Result<String, UiDocError> {
    text.get(key)
        .map(str::to_owned)
        .ok_or_else(|| UiDocError::UnknownTextKey {
            origin: SourceUri(PAGES.to_owned()),
            key: key.to_owned(),
            path: path.to_owned(),
        })
}
