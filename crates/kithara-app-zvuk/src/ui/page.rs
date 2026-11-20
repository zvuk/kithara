use kithara_app_library::{Document, Endpoint, SourcePage};
use kithara_ui::{
    error::UiDocError,
    ids::SourceUri,
    registry::{
        EndpointCategory::{Command, Model},
        ValueKind::{Bool, Text, Trigger},
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

pub(super) fn section() -> Result<FillDocument, UiDocError> {
    FillDocument::parse(
        include_str!("../../assets/zvuk-account.kmodule.ron"),
        SourceUri(consts::SECTION.to_owned()),
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
            (Model, "scope_tracks", Bool),
            (Model, "scope_artists", Bool),
            (Model, "scope_albums", Bool),
            (Command, "scope_tracks", Trigger),
            (Command, "scope_artists", Trigger),
            (Command, "scope_albums", Trigger),
            (Model, "search_controls_hidden", Bool),
            (Model, "back_hidden", Bool),
            (Command, "back", Trigger),
            (Model, "load_more_hidden", Bool),
            (Command, "load_more", Trigger),
            (Model, "suggestions_hidden", Bool),
            (Model, "suggestion_0", Text),
            (Model, "suggestion_1", Text),
            (Model, "suggestion_2", Text),
            (Model, "suggestion_0_hidden", Bool),
            (Model, "suggestion_1_hidden", Bool),
            (Model, "suggestion_2_hidden", Bool),
            (Command, "suggestion_0", Trigger),
            (Command, "suggestion_1", Trigger),
            (Command, "suggestion_2", Trigger),
            (Command, "open_entity", Text),
            (Model, "adaptive_streaming", Bool),
            (Command, "adaptive_streaming", Trigger),
            (Model, "streaming_pending", Bool),
            (Model, "streaming_idle", Bool),
            (Model, "streaming_fault", Text),
            (Model, "streaming_fault_hidden", Bool),
            (Command, "like_track", Text),
            (Model, "fault", Text),
            (Model, "fault_hidden", Bool),
            (Model, "account_label", Text),
            (Model, "account_label_hidden", Bool),
            (Model, "account_fault", Text),
            (Model, "account_fault_hidden", Bool),
            (Model, "account_connect_hidden", Bool),
            (Model, "account_awaiting_hidden", Bool),
            (Model, "account_disconnect_hidden", Bool),
            (Command, "account_connect", Trigger),
            (Command, "account_cancel", Trigger),
            (Command, "account_disconnect", Trigger),
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
