use kithara_app_library::{Document, Endpoint, SourcePage};
use kithara_ui::{
    error::UiDocError,
    ids::SourceUri,
    registry::{
        EndpointCategory::{Command, Model},
        ValueKind::{Bool, Text},
    },
    source::FillDocument,
};

use super::consts;

pub(super) fn document() -> Result<FillDocument, UiDocError> {
    FillDocument::parse(
        include_str!("../../assets/zvuk-page.kmodule.ron"),
        SourceUri(consts::PAGE.to_owned()),
    )
}

pub(super) fn page() -> SourcePage {
    SourcePage {
        id: consts::ID,
        texts: vec![Document {
            path: "texts/zvuk-en.ktext.ron",
            text: include_str!("../../assets/zvuk-en.ktext.ron"),
        }],
        endpoints: [
            (Model, "query", Text),
            (Command, "query", Text),
            (Model, "count", Text),
            (Command, "like_track", Text),
            (Model, "fault", Text),
            (Model, "fault_hidden", Bool),
        ]
        .into_iter()
        .map(|(category, name, value)| Endpoint {
            category,
            name,
            value,
        })
        .collect(),
    }
}
