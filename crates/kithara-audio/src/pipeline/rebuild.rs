use kithara_stream::MediaInfo;

pub(crate) struct RecreateState {
    pub(crate) media_info: Option<MediaInfo>,
    pub(crate) offset: u64,
    pub(crate) cause: RecreateCause,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecreateCause {
    FormatBoundary,
    HostRateChange,
    VariantSwitch,
}
