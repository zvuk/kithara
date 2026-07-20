use std::{
    collections::{BTreeMap, VecDeque},
    sync::LazyLock,
};

use kithara_platform::time::Instant;
use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ControlAction, ReadValue, Reads, StereoLevels, TrackRow, WaveBucket, WaveformView},
};
use num_traits::cast::AsPrimitive;
use serde::Deserialize;

struct Consts;

impl Consts {
    const BPM: &str = "70.00";
    const BPM_VALUE: f32 = 70.0;
    const DOWNBEATS: &[f32] = &[0.0, 0.5];
    const DURATION_SECS: f64 = 360.0;
    const FRAME_WINDOW: usize = 300;
    const KEY: &str = "4m";
    const POSITION_SECS: f64 = 103.0;
    const REMAIN: &str = "−04:17";
    const STRESS_WAVE_BUCKETS: u16 = 8_192;
    const WAVE_BEATS: &[f32] = &[0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875];
}

#[derive(Deserialize)]
struct MockData {
    title: String,
    artist: String,
    tracks: Vec<MockTrack>,
}

#[derive(Deserialize)]
struct MockTrack {
    title: String,
    artist: String,
    time: String,
    search: String,
}

struct Catalog {
    title: &'static str,
    artist: &'static str,
    rows: &'static [TrackRow<'static>],
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(load_catalog);

fn load_catalog() -> Catalog {
    let data: MockData = ron::from_str(include_str!("assets/mock-data.ron"))
        .expect("embedded gallery mock data must parse");
    let data: &'static MockData = Box::leak(Box::new(data));
    let rows: Vec<TrackRow<'static>> = data
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| TrackRow {
            title: &track.title,
            artist: Some(&track.artist),
            time: Some(&track.time),
            search: Some(&track.search),
            current: index == 0,
            selected: index == 0,
        })
        .collect();
    Catalog {
        title: &data.title,
        artist: &data.artist,
        rows: Box::leak(rows.into_boxed_slice()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Tab {
    Atoms,
    Buttons,
    Faders,
    Modules,
    Stress,
}

impl Tab {
    pub(super) const ALL: [Self; 5] = [
        Self::Atoms,
        Self::Buttons,
        Self::Faders,
        Self::Modules,
        Self::Stress,
    ];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Atoms => "gallery-atoms.klayout.ron",
            Self::Buttons => "gallery-buttons.klayout.ron",
            Self::Faders => "gallery-faders.klayout.ron",
            Self::Modules => "gallery-modules.klayout.ron",
            Self::Stress => "gallery-stress.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Atoms => 0,
            Self::Buttons => 1,
            Self::Faders => 2,
            Self::Modules => 3,
            Self::Stress => 4,
        }
    }
}

impl TryFrom<&str> for Tab {
    type Error = ();

    fn try_from(path: &str) -> Result<Self, ()> {
        match path {
            "gallery/atoms" => Ok(Self::Atoms),
            "gallery/buttons" => Ok(Self::Buttons),
            "gallery/faders" => Ok(Self::Faders),
            "gallery/modules" => Ok(Self::Modules),
            "gallery/stress" => Ok(Self::Stress),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ModuleDemo {
    Deck,
    DeckMicro,
    GlobalBar,
    Telemetry,
    Layout,
}

impl ModuleDemo {
    pub(super) const ALL: [Self; 5] = [
        Self::Deck,
        Self::DeckMicro,
        Self::GlobalBar,
        Self::Telemetry,
        Self::Layout,
    ];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Deck => "gallery-modules.klayout.ron",
            Self::DeckMicro => "gallery-modules-deck-micro.klayout.ron",
            Self::GlobalBar => "gallery-modules-global-bar.klayout.ron",
            Self::Telemetry => "gallery-modules-telemetry.klayout.ron",
            Self::Layout => "gallery-modules-layout.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Deck => 0,
            Self::DeckMicro => 1,
            Self::GlobalBar => 2,
            Self::Telemetry => 3,
            Self::Layout => 4,
        }
    }
}

pub(super) struct MockReads {
    active_module: ModuleDemo,
    active_tab: Tab,
    button_cue: bool,
    button_play: bool,
    button_sync: bool,
    checkbox_off: bool,
    checkbox_on: bool,
    chip_active: bool,
    chip_inactive: bool,
    frame_ms: VecDeque<f64>,
    frame_ms_ordered: Vec<f64>,
    frame_ms_avg: String,
    frame_ms_p99: String,
    fps: String,
    knob: f64,
    last_tick: Option<Instant>,
    levels_volume: f64,
    library_query: String,
    playing: bool,
    position_secs: f64,
    toggle_off: bool,
    toggle_on: bool,
    volume: f64,
    waveform: Vec<WaveBucket>,
    stress_fader: f64,
    stress_levels: [StereoLevels; 8],
    stress_phase: f32,
    stress_waveforms: [Vec<WaveBucket>; 4],
}

impl Default for MockReads {
    fn default() -> Self {
        Self {
            active_module: ModuleDemo::Deck,
            active_tab: Tab::Atoms,
            button_cue: false,
            button_play: false,
            button_sync: true,
            checkbox_off: false,
            checkbox_on: true,
            chip_active: true,
            chip_inactive: false,
            frame_ms: VecDeque::with_capacity(Consts::FRAME_WINDOW),
            frame_ms_ordered: Vec::with_capacity(Consts::FRAME_WINDOW),
            frame_ms_avg: "--".to_owned(),
            frame_ms_p99: "--".to_owned(),
            fps: "--".to_owned(),
            knob: 0.5,
            last_tick: None,
            levels_volume: 0.7,
            library_query: String::new(),
            playing: true,
            position_secs: Consts::POSITION_SECS,
            toggle_off: false,
            toggle_on: true,
            volume: 0.7,
            waveform: waveform(),
            stress_fader: 0.7,
            stress_levels: [StereoLevels::default(); 8],
            stress_phase: 0.0,
            stress_waveforms: std::array::from_fn(stress_waveform),
        }
    }
}

impl MockReads {
    pub(super) const fn active_module(&self) -> ModuleDemo {
        self.active_module
    }

    pub(super) const fn active_tab(&self) -> Tab {
        self.active_tab
    }

    pub(super) fn select_tab(&mut self, tab: Tab) {
        if self.active_tab != tab {
            self.last_tick = None;
        }
        self.active_tab = tab;
    }

    pub(super) fn tick(&mut self) {
        if self.active_tab != Tab::Stress {
            return;
        }
        let now = Instant::now();
        if let Some(previous) = self.last_tick.replace(now) {
            self.record_frame(now.duration_since(previous).as_secs_f64() * 1_000.0);
        }
        self.push_stress_data();
    }

    pub(super) fn set_library_query(&mut self, query: String) {
        self.library_query = query;
    }

    pub(super) fn apply(&mut self, path: &str, action: &ControlAction) {
        match action {
            ControlAction::SetScalar(value) => self.set_scalar(path, *value),
            ControlAction::Activate => self.activate(path),
            _ => {}
        }
    }

    fn set_scalar(&mut self, path: &str, value: f64) {
        let value = value.clamp(0.0, 1.0);
        if path.starts_with("atoms/knobs/") {
            self.knob = value;
        } else if path.starts_with("atoms/meters/") {
            self.levels_volume = value;
        } else if path.starts_with("faders/") || path.ends_with("/volume") {
            self.volume = value;
        } else if path == "stress/master" {
            self.stress_fader = value;
        } else if path.ends_with("/wave") {
            self.position_secs = value * Consts::DURATION_SECS;
        }
    }

    fn activate(&mut self, path: &str) {
        match path {
            "modules/selector/deck" => self.active_module = ModuleDemo::Deck,
            "modules/selector/deck-micro" => self.active_module = ModuleDemo::DeckMicro,
            "modules/selector/global-bar" => self.active_module = ModuleDemo::GlobalBar,
            "modules/selector/telemetry" => self.active_module = ModuleDemo::Telemetry,
            "modules/selector/layout" => self.active_module = ModuleDemo::Layout,
            "atoms/toggles/toggle-on" => self.toggle_on = !self.toggle_on,
            "atoms/toggles/toggle-off" => self.toggle_off = !self.toggle_off,
            "atoms/toggles/checkbox-on" => self.checkbox_on = !self.checkbox_on,
            "atoms/toggles/checkbox-off" => self.checkbox_off = !self.checkbox_off,
            "atoms/chips/active" => self.chip_active = !self.chip_active,
            "atoms/chips/inactive" => self.chip_inactive = !self.chip_inactive,
            "buttons/play" => self.button_play = !self.button_play,
            "buttons/cue" => self.button_cue = !self.button_cue,
            "buttons/sync" => self.button_sync = !self.button_sync,
            path if path.ends_with("/play") => self.playing = !self.playing,
            _ => {}
        }
    }
}

impl Reads for MockReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let value = match endpoint {
            "gallery.label.knobs" => ReadValue::Text("KNOB · 26 / 28 / 34 / 38"),
            "gallery.label.meters" => ReadValue::Text("VU · STEREO / VERTICAL"),
            "gallery.label.toggles" => ReadValue::Text("TOGGLE / CHECKBOX"),
            "gallery.label.readouts" => ReadValue::Text("READOUT"),
            "gallery.label.chips" => ReadValue::Text("CHIP"),
            "gallery.label.transport" => ReadValue::Text("TRANSPORT BUTTONS"),
            "gallery.label.regular" => ReadValue::Text("BUTTON STYLES"),
            "gallery.label.text" => ReadValue::Text("TEXT STYLES"),
            "gallery.label.faders" => ReadValue::Text("HORIZONTAL FADERS"),
            "gallery.label.scalar" => ReadValue::Text("SCALAR TELEMETRY"),
            "gallery.tab.atoms" => ReadValue::Bool(self.active_tab == Tab::Atoms),
            "gallery.tab.buttons" => ReadValue::Bool(self.active_tab == Tab::Buttons),
            "gallery.tab.faders" => ReadValue::Bool(self.active_tab == Tab::Faders),
            "gallery.tab.modules" => ReadValue::Bool(self.active_tab == Tab::Modules),
            "gallery.tab.stress" => ReadValue::Bool(self.active_tab == Tab::Stress),
            "gallery.module.deck" => ReadValue::Bool(self.active_module == ModuleDemo::Deck),
            "gallery.module.deck_micro" => {
                ReadValue::Bool(self.active_module == ModuleDemo::DeckMicro)
            }
            "gallery.module.global_bar" => {
                ReadValue::Bool(self.active_module == ModuleDemo::GlobalBar)
            }
            "gallery.module.telemetry" => {
                ReadValue::Bool(self.active_module == ModuleDemo::Telemetry)
            }
            "gallery.module.layout" => ReadValue::Bool(self.active_module == ModuleDemo::Layout),
            "bench.fps" => ReadValue::Text(&self.fps),
            "bench.frame_ms_avg" => ReadValue::Text(&self.frame_ms_avg),
            "bench.frame_ms_p99" => ReadValue::Text(&self.frame_ms_p99),
            "bench.fader" => ReadValue::Scalar(self.stress_fader),
            "bench.wave.0" => stress_waveform_value(&self.stress_waveforms[0]),
            "bench.wave.1" => stress_waveform_value(&self.stress_waveforms[1]),
            "bench.wave.2" => stress_waveform_value(&self.stress_waveforms[2]),
            "bench.wave.3" => stress_waveform_value(&self.stress_waveforms[3]),
            "bench.level.0" => ReadValue::Stereo(self.stress_levels[0]),
            "bench.level.1" => ReadValue::Stereo(self.stress_levels[1]),
            "bench.level.2" => ReadValue::Stereo(self.stress_levels[2]),
            "bench.level.3" => ReadValue::Stereo(self.stress_levels[3]),
            "bench.level.4" => ReadValue::Stereo(self.stress_levels[4]),
            "bench.level.5" => ReadValue::Stereo(self.stress_levels[5]),
            "bench.level.6" => ReadValue::Stereo(self.stress_levels[6]),
            "bench.level.7" => ReadValue::Stereo(self.stress_levels[7]),
            "deck.playback.playing" => ReadValue::Bool(self.playing),
            "deck.playback.position_normalized" => {
                ReadValue::Scalar(self.position_secs / Consts::DURATION_SECS)
            }
            "deck.playback.remaining_secs" => {
                ReadValue::Scalar(Consts::DURATION_SECS - self.position_secs)
            }
            "deck.playback.position_secs" => ReadValue::Scalar(self.position_secs),
            "deck.playback.duration_secs" => ReadValue::Scalar(Consts::DURATION_SECS),
            "deck.playback.waveform" => ReadValue::Waveform(WaveformView {
                buckets: &self.waveform,
                beats: Consts::WAVE_BEATS,
                downbeats: Consts::DOWNBEATS,
                bpm: Some(Consts::BPM_VALUE),
            }),
            "deck.track.title" | "mock.track.title" => ReadValue::Text(CATALOG.title),
            "deck.track.source_kind" | "mock.track.artist" => ReadValue::Text(CATALOG.artist),
            "player.output.volume" | "mock.volume" => ReadValue::Scalar(self.volume),
            "library.visible_tracks" => ReadValue::TrackList(CATALOG.rows),
            "library.query" => ReadValue::Text(&self.library_query),
            "ui.preset" => ReadValue::Text("player"),
            "mock.bpm" => ReadValue::Text(Consts::BPM),
            "mock.key" => ReadValue::Text(Consts::KEY),
            "mock.remain" => ReadValue::Text(Consts::REMAIN),
            "mock.knob" => ReadValue::Scalar(self.knob),
            "mock.levels" => ReadValue::Stereo(StereoLevels {
                l: 0.66,
                r: 0.52,
                volume: self.levels_volume.as_(),
            }),
            "mock.toggle.on" => ReadValue::Bool(self.toggle_on),
            "mock.toggle.off" => ReadValue::Bool(self.toggle_off),
            "mock.checkbox.on" => ReadValue::Bool(self.checkbox_on),
            "mock.checkbox.off" => ReadValue::Bool(self.checkbox_off),
            "mock.chip.active" => ReadValue::Bool(self.chip_active),
            "mock.chip.inactive" => ReadValue::Bool(self.chip_inactive),
            "mock.button.play" => ReadValue::Bool(self.button_play),
            "mock.button.cue" => ReadValue::Bool(self.button_cue),
            "mock.button.sync" => ReadValue::Bool(self.button_sync),
            _ => return None,
        };
        Some(value)
    }
}

impl MockReads {
    fn record_frame(&mut self, frame_ms: f64) {
        if self.frame_ms.len() == Consts::FRAME_WINDOW {
            self.frame_ms.pop_front();
        }
        self.frame_ms.push_back(frame_ms);
        let count = u32::try_from(self.frame_ms.len()).map_or(1.0, f64::from);
        let average = self.frame_ms.iter().sum::<f64>() / count;
        self.frame_ms_ordered.clear();
        self.frame_ms_ordered.extend(self.frame_ms.iter().copied());
        self.frame_ms_ordered.sort_by(f64::total_cmp);
        let percentile = self
            .frame_ms_ordered
            .len()
            .saturating_mul(99)
            .div_ceil(100)
            .saturating_sub(1);
        let Some(p99) = self.frame_ms_ordered.get(percentile).copied() else {
            return;
        };
        self.fps = format!("{:.1}", 1_000.0 / average);
        self.frame_ms_avg = format!("{average:.2}");
        self.frame_ms_p99 = format!("{p99:.2}");
    }

    fn push_stress_data(&mut self) {
        self.stress_phase += 0.037;
        for (index, waveform) in self.stress_waveforms.iter_mut().enumerate() {
            waveform.rotate_left(1);
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            if let Some(bucket) = waveform.last_mut() {
                *bucket = stress_bucket(self.stress_phase + offset * 0.71);
            }
        }
        for (index, levels) in self.stress_levels.iter_mut().enumerate() {
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            let carrier = (self.stress_phase * 2.3 + offset * 0.47).sin();
            let noise = (self.stress_phase * 31.7 + offset * 7.13).sin();
            levels.l = (carrier.mul_add(0.32, noise * 0.08 + 0.54)).clamp(0.0, 1.0);
            levels.r = ((carrier + 0.63).sin().mul_add(0.3, noise * 0.09 + 0.5)).clamp(0.0, 1.0);
            levels.volume = self.stress_fader.as_();
        }
    }
}

fn waveform() -> Vec<WaveBucket> {
    (0_u16..160)
        .map(|index| WaveBucket {
            low: 0.25 + f32::from((index * 17) % 70) / 100.0,
            mid: 0.18 + f32::from((index * 29 + 11) % 65) / 100.0,
            high: 0.12 + f32::from((index * 41 + 23) % 55) / 100.0,
        })
        .collect()
}

fn stress_waveform(index: usize) -> Vec<WaveBucket> {
    let offset = u16::try_from(index).map_or(0.0, f32::from);
    (0..Consts::STRESS_WAVE_BUCKETS)
        .map(|bucket| stress_bucket(f32::from(bucket).mul_add(0.013, offset)))
        .collect()
}

fn stress_bucket(phase: f32) -> WaveBucket {
    WaveBucket {
        low: phase.sin().mul_add(0.34, 0.52).clamp(0.0, 1.0),
        mid: (phase * 1.73).sin().mul_add(0.29, 0.45).clamp(0.0, 1.0),
        high: (phase * 3.11).sin().mul_add(0.2, 0.34).clamp(0.0, 1.0),
    }
}

fn stress_waveform_value(waveform: &[WaveBucket]) -> ReadValue<'_> {
    ReadValue::Waveform(WaveformView {
        buckets: waveform,
        beats: &[],
        downbeats: &[],
        bpm: None,
    })
}

#[derive(Default)]
struct MockRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl MockRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for MockRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

pub(super) fn registry() -> impl EndpointRegistry {
    let mut registry = MockRegistry::default();
    for (id, kind) in [
        ("deck.transport.toggle_play", ValueKind::Trigger),
        ("deck.transport.prev", ValueKind::Trigger),
        ("deck.transport.next", ValueKind::Trigger),
        ("deck.transport.seek_normalized", ValueKind::Scalar),
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(kind).with_scope("deck"),
        );
    }
    for (id, kind) in [
        ("deck.playback.playing", ValueKind::Bool),
        ("deck.playback.position_normalized", ValueKind::Scalar),
        ("deck.playback.remaining_secs", ValueKind::Scalar),
        ("deck.playback.position_secs", ValueKind::Scalar),
        ("deck.playback.duration_secs", ValueKind::Scalar),
        ("deck.playback.waveform", ValueKind::Waveform),
        ("deck.track.title", ValueKind::Text),
        ("deck.track.source_kind", ValueKind::Text),
    ] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(kind).with_scope("deck"),
        );
    }
    registry.insert(
        EndpointCategory::Parameter,
        "player.output.volume",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for (id, kind) in [
        ("library.visible_tracks", ValueKind::TrackList),
        ("library.query", ValueKind::Text),
        ("ui.preset", ValueKind::Text),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
    for id in [
        "gallery.label.knobs",
        "gallery.label.meters",
        "gallery.label.toggles",
        "gallery.label.readouts",
        "gallery.label.chips",
        "gallery.label.transport",
        "gallery.label.regular",
        "gallery.label.text",
        "gallery.label.faders",
        "gallery.label.scalar",
        "mock.track.title",
        "mock.track.artist",
        "mock.bpm",
        "mock.key",
        "mock.remain",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    for id in [
        "gallery.tab.atoms",
        "gallery.tab.buttons",
        "gallery.tab.faders",
        "gallery.tab.modules",
        "gallery.tab.stress",
        "gallery.module.deck",
        "gallery.module.deck_micro",
        "gallery.module.global_bar",
        "gallery.module.telemetry",
        "gallery.module.layout",
        "mock.toggle.on",
        "mock.toggle.off",
        "mock.checkbox.on",
        "mock.checkbox.off",
        "mock.chip.active",
        "mock.chip.inactive",
        "mock.button.play",
        "mock.button.cue",
        "mock.button.sync",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool),
        );
    }
    for id in ["mock.knob", "mock.volume"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    registry.insert(
        EndpointCategory::Model,
        "mock.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
    for id in ["bench.fps", "bench.frame_ms_avg", "bench.frame_ms_p99"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    registry.insert(
        EndpointCategory::Model,
        "bench.fader",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for index in 0..4 {
        registry.insert(
            EndpointCategory::Model,
            &format!("bench.wave.{index}"),
            EndpointDesc::new(ValueKind::Waveform),
        );
    }
    for index in 0..8 {
        registry.insert(
            EndpointCategory::Model,
            &format!("bench.level.{index}"),
            EndpointDesc::new(ValueKind::Stereo),
        );
    }
    registry
}
