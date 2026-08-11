//! Two real records, analysed by the production analyser and beat-matched the
//! way the studio's SYNC does it, must strike their beats together.
//!
//! The synthetic oracles in `kithara_app::beatmatch` prove the arithmetic. They
//! cannot prove the thing a DJ actually cares about, because a click track has
//! an even grid by construction: real markers drift, real detectors pick the
//! wrong octave, and a tempo that is right on average is wrong by the end of a
//! phrase. That is what these measure.
//!
//! They need a music library, so they name their two records through the
//! environment and are `#[ignore]`d — an absent library must not read as a
//! pass. Run one with:
//!
//! ```text
//! KITHARA_BEATMATCH_A=/path/one.mp3 KITHARA_BEATMATCH_B=/path/two.flac \
//!   cargo test -p kithara-integration-tests --test suite_light \
//!   beat_match -- --ignored --nocapture
//! ```

#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{BeatGrid, analysis::BeatAnalysisConfig},
    bufpool::{BytePool, PcmPool},
    platform::{CancelToken, time::Duration},
    prelude::ResourceConfig,
};
use kithara_app::{
    beatmatch,
    waveform::{TrackAnalysis, TrackAnalysisRunner},
};
use kithara_integration_tests::memory_asset_store;
use num_traits::ToPrimitive;

struct Consts;

impl Consts {
    /// Session beats measured. Two phrases of four bars: long enough that a
    /// tempo wrong in the third decimal shows up as a flam by the end.
    const MEASURED_BEATS: usize = 32;
    /// How far apart two strikes may land and still read as one. Ten
    /// milliseconds is about where a DJ starts to hear a flam rather than a
    /// thicker drum.
    const FLAM_SECONDS: f64 = 0.010;
    /// The frame the detector places its marks on, measured: gaps on both
    /// records snapped between 460 and 480 ms, so a mark can sit 10 ms either
    /// side of the beat it names. Nothing measured against those marks can be
    /// tighter than this, and a tolerance below it would be measuring the
    /// detector's resolution rather than the deck's timing.
    const DETECTOR_FRAME_SECONDS: f64 = 0.020;
    /// Waveform buckets the runner is asked for. The beat pass is what this
    /// wants; the envelope is incidental.
    const BUCKETS: usize = 2_000;
    /// Seconds in a minute, converting between BPM and beat length.
    const SECONDS_PER_MINUTE: f64 = 60.0;
}

/// The two records under test, or `None` when the environment names none.
fn records() -> Option<(String, String)> {
    let a = std::env::var("KITHARA_BEATMATCH_A").ok()?;
    let b = std::env::var("KITHARA_BEATMATCH_B").ok()?;
    Some((a, b))
}

/// Analyse one record through the production runner, exactly as the app does.
async fn analyse(path: &str) -> TrackAnalysis {
    let src = ResourceConfig::parse_src(path)
        .unwrap_or_else(|error| panic!("{path} must name a source: {error}"));
    let config = ResourceConfig::for_src(src)
        .store(memory_asset_store())
        .byte_pool(BytePool::default())
        .pcm_pool(PcmPool::default())
        .build();

    let mut runner = TrackAnalysisRunner::new(
        &CancelToken::never(),
        Consts::BUCKETS,
        BeatAnalysisConfig::default(),
        PcmPool::default(),
    );
    let mut rx = runner.analyze(config, std::sync::Arc::from(path));

    // Staged analysis emits the envelope first and the beat grid after it.
    let mut last = None;
    while rx.changed().await.is_ok() {
        last = rx.borrow().clone();
    }
    last.unwrap_or_else(|| panic!("{path} produced no analysis at all"))
}

/// One deck: the record's own marked beats, the tempo the app read off them,
/// and the speed SYNC puts it at.
struct Deck {
    label: String,
    /// Marked beat instants in source seconds, ascending.
    marks: Vec<f64>,
    bpm: f64,
    speed: f64,
}

impl Deck {
    fn new(label: &str, analysis: &TrackAnalysis, session_bpm: f64) -> Self {
        let rate = analysis
            .source_sample_rate()
            .unwrap_or_else(|| panic!("{label} carries no source rate"));
        let grid = analysis
            .beat()
            .unwrap_or_else(|| panic!("{label} produced no beat grid"));
        let bpm = beatmatch::deck_bpm(analysis)
            .unwrap_or_else(|| panic!("{label} produced no usable tempo"));
        let marks = grid
            .beats()
            .iter()
            .filter_map(|frame| Some(frame.to_f64()? / f64::from(rate.get())))
            .collect();
        let speed = beatmatch::speed(session_bpm, bpm)
            .unwrap_or_else(|| panic!("{label} at {bpm} BPM cannot be matched to {session_bpm}"));
        Self {
            label: label.to_owned(),
            marks,
            bpm,
            speed,
        }
    }

    /// The first mark at or after a quarter of the record, so a measurement
    /// starts inside the music rather than in an intro the detector guessed at.
    fn origin(&self) -> f64 {
        let quarter = self.marks.len() / 4;
        *self
            .marks
            .get(quarter)
            .unwrap_or_else(|| panic!("{} has too few marks to measure", self.label))
    }

    /// Where the deck actually strikes, in session seconds.
    ///
    /// A deck does not play the marks: SYNC sets one speed and the deck runs at
    /// it, so its beats are evenly spaced whatever the detector scattered. That
    /// is the thing to measure — the marks are the record, this is the deck.
    fn strikes(&self, beats: usize) -> Vec<f64> {
        let period = Consts::SECONDS_PER_MINUTE / self.bpm / self.speed;
        (0..beats)
            .filter_map(|beat| Some(beat.to_f64()? * period))
            .collect()
    }

    /// Distance from each strike to the nearest mark of this same record.
    ///
    /// Measured strike-to-mark and not the other way round on purpose: an
    /// extra mark the detector invented has no strike near it and is not this
    /// deck's fault, while a strike with no mark under it is exactly the deck
    /// playing off its own music.
    fn off_its_own_beats(&self, beats: usize) -> Vec<f64> {
        let origin = self.origin();
        self.strikes(beats)
            .iter()
            .map(|strike| {
                let at = origin + strike * self.speed;
                self.marks
                    .iter()
                    .map(|mark| (mark - at).abs())
                    .fold(f64::INFINITY, f64::min)
                    / self.speed
            })
            .collect()
    }

    /// The worst a strike misses its record's marks.
    fn worst_off(&self, beats: usize) -> f64 {
        self.off_its_own_beats(beats)
            .into_iter()
            .fold(0.0_f64, f64::max)
    }

    /// What a strike typically misses by.
    ///
    /// The middle rather than the worst, because a real detector leaves holes:
    /// a bar of breakdown it marked no beats in puts one strike half a beat
    /// from anything, and a maximum then reports the detector's coverage
    /// instead of the deck's timing.
    fn typically_off(&self, beats: usize) -> f64 {
        let mut offs = self.off_its_own_beats(beats);
        offs.sort_by(f64::total_cmp);
        offs.get(offs.len() / 2).copied().unwrap_or(f64::INFINITY)
    }
}

/// Builds both decks against a session tempo taken from the first record, the
/// way the studio does: the first record on defines the grid.
async fn decks() -> (Deck, Deck) {
    let Some((path_a, path_b)) = records() else {
        panic!("KITHARA_BEATMATCH_A and KITHARA_BEATMATCH_B must both name a record");
    };
    let analysis_a = analyse(&path_a).await;
    let analysis_b = analyse(&path_b).await;
    let session_bpm = beatmatch::deck_bpm(&analysis_a)
        .unwrap_or_else(|| panic!("{path_a} produced no tempo to open the session with"));

    let a = Deck::new(&path_a, &analysis_a, session_bpm);
    let b = Deck::new(&path_b, &analysis_b, session_bpm);
    println!(
        "session {session_bpm:.2} BPM\n  A {:.3} BPM speed {:.5}\n  B {:.3} BPM speed {:.5}",
        a.bpm, a.speed, b.bpm, b.speed
    );
    (a, b)
}

/// Both decks, matched, strike at the same instants.
///
/// This is what "beat-matched" means and it is the headline property: two
/// records at different tempos, put on one grid, land together.
#[ignore = "needs a music library named through KITHARA_BEATMATCH_A / _B"]
#[kithara::test(tokio, timeout(Duration::from_secs(180)))]
async fn two_matched_records_strike_together() {
    let (a, b) = decks().await;
    let strikes_a = a.strikes(Consts::MEASURED_BEATS);
    let strikes_b = b.strikes(Consts::MEASURED_BEATS);

    let worst = strikes_a
        .iter()
        .zip(&strikes_b)
        .map(|(one, other)| (one - other).abs())
        .fold(0.0_f64, f64::max);
    println!(
        "worst separation {:.3} ms over {} beats",
        worst * 1000.0,
        Consts::MEASURED_BEATS
    );
    assert!(
        worst <= Consts::FLAM_SECONDS,
        "{} and {} drift {:.1} ms apart over {} beats",
        a.label,
        b.label,
        worst * 1000.0,
        Consts::MEASURED_BEATS
    );
}

/// A deck running at the tempo the app read off a record does not walk away
/// from that record's own beats.
///
/// This is the one real music can fail and a click track cannot: a tempo wrong
/// in the second decimal is inaudible on one beat and a quarter-beat out after
/// thirty-two. Measured at both ends, and compared to each other — a per-beat
/// tempo error multiplies with the beat count, while the detector's placement
/// noise does not, and that difference is what tells the two apart.
#[ignore = "needs a music library named through KITHARA_BEATMATCH_A / _B"]
#[kithara::test(tokio, timeout(Duration::from_secs(180)))]
async fn the_estimated_tempo_does_not_walk_off_each_record() {
    let (a, b) = decks().await;
    for deck in [&a, &b] {
        let quarter = Consts::MEASURED_BEATS / 4;
        let early = deck.worst_off(quarter);
        let late = deck.worst_off(Consts::MEASURED_BEATS);
        println!(
            "{}: {:.1} ms off after {quarter} beats, {:.1} ms after {}",
            deck.label,
            early * 1000.0,
            late * 1000.0,
            Consts::MEASURED_BEATS
        );
        assert!(
            late - early <= Consts::DETECTOR_FRAME_SECONDS,
            "{} walks off its own beats: {:.1} ms after {quarter} beats, {:.1} ms after {}",
            deck.label,
            early * 1000.0,
            late * 1000.0,
            Consts::MEASURED_BEATS
        );
    }
}

/// The deck sits on its record's beats to within the detector's own resolution.
///
/// Separate from the drift above on purpose: a tempo can be stable and still
/// be the wrong tempo, and one assertion covering both would prove neither.
#[ignore = "needs a music library named through KITHARA_BEATMATCH_A / _B"]
#[kithara::test(tokio, timeout(Duration::from_secs(180)))]
async fn each_deck_sits_on_its_own_record_s_beats() {
    let (a, b) = decks().await;
    for deck in [&a, &b] {
        let typical = deck.typically_off(Consts::MEASURED_BEATS);
        println!("{}: typically {:.1} ms off", deck.label, typical * 1000.0);
        assert!(
            typical <= Consts::DETECTOR_FRAME_SECONDS,
            "{} sits {:.1} ms off its own beats, past the {:.0} ms its marks are placed to",
            deck.label,
            typical * 1000.0,
            Consts::DETECTOR_FRAME_SECONDS * 1000.0
        );
    }
}

/// The tempo the markers actually march at, over the span they cover.
fn marker_tempo(grid: &BeatGrid, rate: f64) -> Option<f64> {
    let beats = grid.beats();
    let first = *beats.first()?;
    let last = *beats.last()?;
    let intervals = (beats.len() - 1).to_f64()?;
    let seconds = (last - first).to_f64()? / rate;
    (seconds > 0.0).then(|| intervals * 60.0 / seconds)
}

/// The grid the analyser found, printed rather than asserted: the record's own
/// tempo is not this crate's to decide, and a run that reports it is how a
/// wrong octave gets noticed.
#[ignore = "needs a music library named through KITHARA_BEATMATCH_A / _B"]
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn report_what_the_analyser_made_of_both_records() {
    let Some((path_a, path_b)) = records() else {
        panic!("KITHARA_BEATMATCH_A and KITHARA_BEATMATCH_B must both name a record");
    };
    for path in [path_a, path_b] {
        let analysis = analyse(&path).await;
        let rate = f64::from(
            analysis
                .source_sample_rate()
                .unwrap_or_else(|| panic!("{path} carries no source rate"))
                .get(),
        );
        let grid = analysis
            .beat()
            .unwrap_or_else(|| panic!("{path} produced no beat grid"));
        let mut gaps: Vec<f64> = grid
            .beats()
            .windows(2)
            .filter_map(|pair| (pair[1] - pair[0]).to_f64())
            .map(|frames| frames / rate * 1000.0)
            .collect();
        gaps.sort_by(f64::total_cmp);
        let at = |q: f64| {
            gaps.get((gaps.len().to_f64().unwrap_or_default() * q) as usize)
                .copied()
        };
        println!(
            "{path}\n  frames {} rate {rate} reported {:.3} BPM, {} markers\n  \
             marks span {:?} BPM, app reads {:?} BPM\n  \
             gap ms p05 {:?} p25 {:?} p50 {:?} p75 {:?} p95 {:?}",
            analysis.source_frames(),
            grid.bpm(),
            grid.beats().len(),
            marker_tempo(grid, rate).map(|bpm| format!("{bpm:.2}")),
            beatmatch::deck_bpm(&analysis).map(|bpm| format!("{bpm:.2}")),
            at(0.05).map(|ms| format!("{ms:.0}")),
            at(0.25).map(|ms| format!("{ms:.0}")),
            at(0.50).map(|ms| format!("{ms:.0}")),
            at(0.75).map(|ms| format!("{ms:.0}")),
            at(0.95).map(|ms| format!("{ms:.0}")),
        );
    }
}

/// The tempo the analysis *reports* is the tempo its own markers march at.
///
/// These are two different fields of one grid, and everything downstream picks
/// one of them: the deck's BPM readout and SYNC's tempo match read the scalar,
/// while the phase and the beat map read the markers. A grid whose halves
/// disagree beat-matches to a tempo none of its beats are on.
#[ignore = "needs a music library named through KITHARA_BEATMATCH_A / _B"]
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn the_reported_tempo_is_the_tempo_the_markers_march_at() {
    /// Beat multiples apart still counts as agreement: half-time is a reading
    /// of the same grid, a factor of 3.8 is not.
    const TOLERANCE: f64 = 0.02;

    let Some((path_a, path_b)) = records() else {
        panic!("KITHARA_BEATMATCH_A and KITHARA_BEATMATCH_B must both name a record");
    };
    for path in [path_a, path_b] {
        let analysis = analyse(&path).await;
        let rate = f64::from(
            analysis
                .source_sample_rate()
                .unwrap_or_else(|| panic!("{path} carries no source rate"))
                .get(),
        );
        let grid = analysis
            .beat()
            .unwrap_or_else(|| panic!("{path} produced no beat grid"));
        let marched = marker_tempo(grid, rate)
            .unwrap_or_else(|| panic!("{path} has too few markers to name a tempo"));
        let folded = beatmatch::speed(grid.bpm(), marched)
            .unwrap_or_else(|| panic!("{path} names a tempo pair that cannot be folded"));
        println!(
            "{path}: reported {:.3}, marched {marched:.3}, folded {folded:.5}",
            grid.bpm()
        );
        assert!(
            (folded - 1.0).abs() <= TOLERANCE,
            "{path} reports {:.3} BPM while its {} markers march at {marched:.3} BPM",
            grid.bpm(),
            grid.beats().len()
        );
    }
}
