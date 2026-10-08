use kithara_ui::{
    error::UiDocError,
    registry::{EndpointCategory, ValueKind},
    source::FillDocument,
    text::TextDoc,
};

use crate::LibrarySource;

/// A document a source brings into the package.
#[derive(Clone, Copy)]
pub struct Document {
    /// The package-relative path the document is named by.
    pub path: &'static str,
    pub text: &'static str,
}

/// Source id, UI endpoints, and caption catalogs.
pub struct SourcePage {
    /// Names the source; its page reads and writes are scoped by it.
    pub id: &'static str,
    /// Reads and writes the source's page declares.
    pub endpoints: Vec<Endpoint>,
    /// Caption catalogs laid over the package's own.
    pub texts: Vec<Document>,
}

/// A read or write a source's page declares. The shell registers it as
/// `source.<name>` scoped by `source` and routes it to the scoped source.
#[derive(Clone, Copy)]
pub struct Endpoint {
    /// One segment, distinct from the shell's own `rows`, `status`, `select`
    /// and `column`.
    pub name: &'static str,
    pub category: EndpointCategory,
    pub value: ValueKind,
}

/// Builds a registered source from the package's text catalog.
type Build = Box<dyn FnOnce(&TextDoc) -> Result<Box<dyn LibrarySource>, UiDocError>>;

/// Source metadata, UI fills, and a builder using the package text catalog.
pub struct Registration {
    build: Build,
    page: SourcePage,
    fills: Vec<(String, FillDocument)>,
}

impl Registration {
    pub fn new<F>(page: SourcePage, build: F) -> Self
    where
        F: FnOnce(&TextDoc) -> Result<Box<dyn LibrarySource>, UiDocError> + 'static,
    {
        Self {
            page,
            build: Box::new(build),
            fills: Vec::new(),
        }
    }

    /// Adds a document to `<module id>/<collection>` under the source id.
    #[must_use]
    pub fn fill(mut self, address: &str, document: FillDocument) -> Self {
        self.fills.push((address.to_owned(), document));
        self
    }

    /// Builds the source once the package's text catalog is known.
    ///
    /// # Errors
    /// Returns the error of a label the catalog does not word.
    pub fn build(self, text: &TextDoc) -> Result<Box<dyn LibrarySource>, UiDocError> {
        (self.build)(text)
    }

    #[must_use]
    pub const fn page(&self) -> &SourcePage {
        &self.page
    }

    /// Collection addresses and documents in registration order.
    pub fn fills(&self) -> impl Iterator<Item = (&str, &FillDocument)> {
        self.fills
            .iter()
            .map(|(address, document)| (address.as_str(), document))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::ids::{DocId, SourceUri};

    use super::*;
    use crate::PAGES;

    #[kithara::test]
    fn a_registration_carries_the_document_it_fills_a_collection_with() {
        let origin = SourceUri("probe-page.kmodule.ron".to_owned());
        let document = FillDocument::parse(
            r#"(schema: "kithara.module", version: 1, id: "probe-page", root: Spacer(id: "face"))"#,
            origin,
        )
        .expect("the page parses");
        let page = SourcePage {
            id: "probe",
            endpoints: Vec::new(),
            texts: Vec::new(),
        };

        let registration = Registration::new(page, |_| {
            Err(UiDocError::NotFound {
                origin: SourceUri("probe".to_owned()),
                rel: String::new(),
            })
        })
        .fill(PAGES, document);

        let fills: Vec<_> = registration.fills().collect();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].0, "app-library/pages");
        assert!(matches!(
            fills[0].1,
            FillDocument::Parsed { document, .. } if document.id == DocId("probe-page".to_owned())
        ));
    }
}
