use kithara_ui::module::{TableColumn, TableColumnStyle};

pub(crate) const BPM: &str = "70.00";
pub(crate) const BPM_VALUE: f32 = 70.0;
pub(crate) const CACHED_NORMALIZED: f64 = 0.47;
pub(crate) const CUES: &[f32] = &[0.27, 0.31];
pub(crate) const DURATION_SECS: f64 = 360.0;
pub(crate) const ENGINE_LOAD: f64 = 0.21;
pub(crate) const KEY: &str = "4m";
pub(crate) const LATENCY: &str = "5.3 MS";
pub(crate) const LOOP_REGION: [f32; 2] = [0.30, 0.34];
/// Where the artwork page's fader starts, and — because a capture never
/// ticks — the frame both hosts photograph the scrubbed artwork at. Far
/// enough along its one second pass to stand at a plainly different frame
/// from the played one beside it.
pub(crate) const LOTTIE_SCRUB_START: f32 = 0.6;
/// Where the demo clock turns over, being the common period of every motion
/// on the page, so a gallery left running keeps its seconds exact in `f32`.
/// A real application hands over its own monotonic time and never wraps.
pub(crate) const MOTION_CLOCK_PERIOD: f32 = 8.0;
/// The second a capture photographs the motion row at. The row's tracks all
/// run four seconds, so this is the same 0.35 of the way along as
/// [`MOTION_START`]: the page shows one journey said two ways.
pub(crate) const MOTION_CLOCK_START: f32 = 1.4;
/// Where every track starts, and — because a capture never ticks — the one
/// phase both hosts are photographed at. Away from either end, so the page
/// compares turned, scaled and moved ink rather than four identities.
pub(crate) const MOTION_START: f32 = 0.35;
/// How far a motion track travels per 16 ms tick: a full pass every two
/// seconds, slow enough to read and fast enough to see.
pub(crate) const MOTION_STEP: f32 = 0.008;
/// Seconds the motion clock advances per 16 ms tick.
pub(crate) const MOTION_TICK_SECS: f32 = 0.016;
pub(crate) const ON_AIR: &str = "ON AIR · DECK A";
pub(crate) const POSITION_SECS: f64 = 103.0;
pub(crate) const RECORD_TIME: &str = "00:42:18";
pub(crate) const REMAIN: &str = "−04:17";
/// Where the sprite page's fader starts, and — because a capture never
/// ticks — the frame both hosts photograph the scrubbed sheet at. Three
/// eighths of the way along, so the scrubbed sprite shows a different frame
/// from the played one beside it.
pub(crate) const SPRITE_SCRUB_START: f32 = 0.375;
pub(crate) const TABLE_LIBRARY: [bool; 9] =
    [true, true, true, true, true, true, true, false, false];
pub(crate) const TABLE_MICRO: [bool; 9] =
    [false, false, true, false, false, false, true, false, false];
pub(crate) const TABLE_QUEUE: [bool; 9] = [true, true, true, false, true, true, false, true, true];
pub(crate) const TABLE_QUEUE_PRESET: usize = 1;
pub(crate) const TEMPO: &str = "+0.0%";
pub(crate) const VIS_TICK_SECS: f64 = 0.016;
pub(crate) const WAVE_BUCKETS: u32 = 4_096;
/// Holes a pass spread over the track has not reached yet.
pub(crate) const WAVE_UNREADY: [[f32; 2]; 5] = [
    [0.09, 0.16],
    [0.30, 0.37],
    [0.44, 0.52],
    [0.58, 0.66],
    [0.79, 0.90],
];
pub(crate) const ZOOM: f64 = 0.12;

pub(crate) fn table_columns() -> [TableColumn; 9] {
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
