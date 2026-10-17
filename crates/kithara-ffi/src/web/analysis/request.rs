use kithara::queue::TrackId;

use crate::web::resources::ResourceBuilder;

pub(in crate::web) struct AnalysisRequest {
    pub(in crate::web) id: TrackId,
    pub(in crate::web) request_id: u32,
    pub(in crate::web) resources: ResourceBuilder,
}
