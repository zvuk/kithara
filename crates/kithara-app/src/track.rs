use kithara_queue::TrackStatus;

pub(crate) enum TrackRow {
    Failed,
    SlowCurrent,
    Slow,
    Current,
    Normal,
}

impl TrackRow {
    pub(crate) fn classify(status: &TrackStatus, is_current: bool) -> Self {
        match (status, is_current) {
            (TrackStatus::Failed(_), _) => Self::Failed,
            (TrackStatus::Slow, true) => Self::SlowCurrent,
            (TrackStatus::Slow, false) => Self::Slow,
            (_, true) => Self::Current,
            (_, false) => Self::Normal,
        }
    }
}
