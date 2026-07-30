use kithara_ui::module::TrackColumn;

pub(super) struct Consts;

impl Consts {
    pub(super) const BPM: &str = "70.00";
    pub(super) const BPM_VALUE: f32 = 70.0;
    pub(super) const CUES: &[f32] = &[0.27, 0.31];
    pub(super) const DURATION_SECS: f64 = 360.0;
    pub(super) const KEY: &str = "4m";
    pub(super) const LOOP_REGION: [f32; 2] = [0.30, 0.34];
    pub(super) const POSITION_SECS: f64 = 103.0;
    pub(super) const REMAIN: &str = "−04:17";
    pub(super) const TEMPO: &str = "+0.0%";
    pub(super) const TRACK_COLUMNS: [TrackColumn; 9] = [
        TrackColumn::Index,
        TrackColumn::Deck,
        TrackColumn::Title,
        TrackColumn::Artist,
        TrackColumn::Bpm,
        TrackColumn::Key,
        TrackColumn::Time,
        TrackColumn::Energy,
        TrackColumn::Transition,
    ];
    pub(super) const TRACKLIST_LIBRARY: [bool; 9] =
        [true, true, true, true, true, true, true, false, false];
    pub(super) const TRACKLIST_MICRO: [bool; 9] =
        [false, false, true, false, false, false, true, false, false];
    pub(super) const TRACKLIST_QUEUE: [bool; 9] =
        [true, true, true, false, true, true, false, true, true];
    pub(super) const TRACKLIST_QUEUE_PRESET: usize = 1;
    pub(super) const VIS_TICK_SECS: f64 = 0.016;
    pub(super) const WAVE_BUCKETS: u32 = 4_096;
    pub(super) const ZOOM: f64 = 0.12;
}
