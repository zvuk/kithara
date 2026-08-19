use kithara_ui::module::{TableColumn, TableColumnStyle};

pub(super) struct Consts;

impl Consts {
    pub(super) const BPM: &str = "70.00";
    /// How far a motion track travels per 16 ms tick: a full pass every two
    /// seconds, slow enough to read and fast enough to see.
    pub(super) const MOTION_STEP: f32 = 0.008;
    /// Where every track starts, and — because a capture never ticks — the one
    /// phase both hosts are photographed at. Away from either end, so the page
    /// compares turned, scaled and moved ink rather than four identities.
    pub(super) const MOTION_START: f32 = 0.35;
    /// Seconds the motion clock advances per 16 ms tick.
    pub(super) const MOTION_TICK_SECS: f32 = 0.016;
    /// The second a capture photographs the motion row at. The row's tracks all
    /// run four seconds, so this is the same 0.35 of the way along as
    /// [`Self::MOTION_START`]: the page shows one journey said two ways.
    pub(super) const MOTION_CLOCK_START: f32 = 1.4;
    /// Where the mock clock turns over, being the common period of every motion
    /// on the page, so a gallery left running keeps its seconds exact in `f32`.
    /// A real application hands over its own monotonic time and never wraps.
    pub(super) const MOTION_CLOCK_PERIOD: f32 = 8.0;
    /// Where the sprite page's fader starts, and — because a capture never
    /// ticks — the frame both hosts photograph the scrubbed sheet at. Three
    /// eighths of the way along, so the scrubbed sprite shows a different frame
    /// from the played one beside it.
    pub(super) const SPRITE_SCRUB_START: f32 = 0.375;
    /// Where the artwork page's fader starts, and — because a capture never
    /// ticks — the frame both hosts photograph the scrubbed artwork at. Far
    /// enough along its one second pass to stand at a plainly different frame
    /// from the played one beside it.
    pub(super) const LOTTIE_SCRUB_START: f32 = 0.6;
    pub(super) const BPM_VALUE: f32 = 70.0;
    pub(super) const CUES: &[f32] = &[0.27, 0.31];
    pub(super) const DURATION_SECS: f64 = 360.0;
    pub(super) const KEY: &str = "4m";
    pub(super) const LOOP_REGION: [f32; 2] = [0.30, 0.34];
    pub(super) const POSITION_SECS: f64 = 103.0;
    pub(super) const REMAIN: &str = "−04:17";
    pub(super) const TEMPO: &str = "+0.0%";
    pub(super) const TABLE_LIBRARY: [bool; 9] =
        [true, true, true, true, true, true, true, false, false];
    pub(super) const TABLE_MICRO: [bool; 9] =
        [false, false, true, false, false, false, true, false, false];
    pub(super) const TABLE_QUEUE: [bool; 9] =
        [true, true, true, false, true, true, false, true, true];
    pub(super) const TABLE_QUEUE_PRESET: usize = 1;
    pub(super) const VIS_TICK_SECS: f64 = 0.016;
    pub(super) const WAVE_BUCKETS: u32 = 4_096;
    pub(super) const ZOOM: f64 = 0.12;

    pub(super) fn table_columns() -> [TableColumn; 9] {
        [
            TableColumn::new("index", "#", TableColumnStyle::Index, 28.0, false),
            TableColumn::new("deck", "DECK", TableColumnStyle::Badge, 64.0, false),
            TableColumn::new("title", "TITLE", TableColumnStyle::Primary, 180.0, true),
            TableColumn::new(
                "artist",
                "ARTIST",
                TableColumnStyle::Secondary,
                200.0,
                false,
            ),
            TableColumn::new("bpm", "BPM", TableColumnStyle::Metric, 70.0, false),
            TableColumn::new("key", "KEY", TableColumnStyle::Mono, 56.0, false),
            TableColumn::new("time", "TIME", TableColumnStyle::Time, 70.0, false),
            TableColumn::new("energy", "ENERGY", TableColumnStyle::Meter, 110.0, false),
            TableColumn::new(
                "transition",
                "TRANSITION",
                TableColumnStyle::Transition,
                130.0,
                false,
            ),
        ]
    }
}
