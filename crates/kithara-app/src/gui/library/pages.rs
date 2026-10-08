use kithara::ui::{
    error::UiDocError,
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc},
    source::{FillDocument, MemResolver},
    text::TextDoc,
};
use kithara_app_library::{Document, LibrarySource, PAGES, Registration, SourcePage};

use crate::gui::ui::endpoints::Registry;

pub(in crate::gui) struct SourceAdditions {
    pub(in crate::gui) fills: MemResolver,
    pub(in crate::gui) registry: Registry,
    pub(in crate::gui) texts: Vec<Document>,
}

impl SourceAdditions {
    pub(in crate::gui) fn new(sources: &[Registration]) -> Self {
        let mut fills = MemResolver::default();
        let mut texts: Vec<Document> = Vec::new();
        let mut endpoints: Vec<(EndpointCategory, EndpointId, EndpointDesc)> = Vec::new();
        for source in sources {
            let page = source.page();
            for (address, document) in source.fills() {
                fills.fill(address, page.id, document.clone());
            }
            texts.extend(page.texts.iter().copied());
            endpoints.extend(page.endpoints.iter().map(|endpoint| {
                (
                    endpoint.category,
                    EndpointId(format!("source.{}", endpoint.name)),
                    EndpointDesc::new(endpoint.value).with_scope("source"),
                )
            }));
        }
        Self {
            fills,
            texts,
            registry: Registry::default().with_endpoints(endpoints),
        }
    }
}

pub(in crate::gui) fn listed<F>(id: &'static str, build: F) -> Registration
where
    F: FnOnce(&TextDoc) -> Result<Box<dyn LibrarySource>, UiDocError> + 'static,
{
    let page = SourcePage {
        id,
        endpoints: Vec::new(),
        texts: Vec::new(),
    };
    Registration::new(page, build).fill(
        PAGES,
        FillDocument::Path("modules/library/source-page.kmodule.ron".to_owned()),
    )
}
