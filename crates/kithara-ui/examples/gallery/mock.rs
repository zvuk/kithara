use std::{collections::BTreeMap, sync::LazyLock};

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
    const KEY: &str = "4m";
    const POSITION_SECS: f64 = 103.0;
    const REMAIN: &str = "−04:17";
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
}

impl Tab {
    pub(super) const ALL: [Self; 4] = [Self::Atoms, Self::Buttons, Self::Faders, Self::Modules];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Atoms => "gallery-atoms.klayout.ron",
            Self::Buttons => "gallery-buttons.klayout.ron",
            Self::Faders => "gallery-faders.klayout.ron",
            Self::Modules => "gallery-modules.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Atoms => 0,
            Self::Buttons => 1,
            Self::Faders => 2,
            Self::Modules => 3,
        }
    }
}

impl TryFrom<&str> for Tab {
    type Error = ();

    fn try_from(path: &str) -> Result<Self, ()> {
        match path {
            "gallery/tab/atoms" => Ok(Self::Atoms),
            "gallery/tab/buttons" => Ok(Self::Buttons),
            "gallery/tab/faders" => Ok(Self::Faders),
            "gallery/tab/modules" => Ok(Self::Modules),
            _ => Err(()),
        }
    }
}

pub(super) struct MockReads {
    active_tab: Tab,
    button_cue: bool,
    button_play: bool,
    button_sync: bool,
    checkbox_off: bool,
    checkbox_on: bool,
    chip_active: bool,
    chip_inactive: bool,
    knob: f64,
    levels_volume: f64,
    library_query: String,
    playing: bool,
    position_secs: f64,
    toggle_off: bool,
    toggle_on: bool,
    volume: f64,
    waveform: Vec<WaveBucket>,
}

impl Default for MockReads {
    fn default() -> Self {
        Self {
            active_tab: Tab::Atoms,
            button_cue: false,
            button_play: false,
            button_sync: true,
            checkbox_off: false,
            checkbox_on: true,
            chip_active: true,
            chip_inactive: false,
            knob: 0.5,
            levels_volume: 0.7,
            library_query: String::new(),
            playing: true,
            position_secs: Consts::POSITION_SECS,
            toggle_off: false,
            toggle_on: true,
            volume: 0.7,
            waveform: waveform(),
        }
    }
}

impl MockReads {
    pub(super) const fn active_tab(&self) -> Tab {
        self.active_tab
    }

    pub(super) fn select_tab(&mut self, tab: Tab) {
        self.active_tab = tab;
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
        } else if path.ends_with("/wave") {
            self.position_secs = value * Consts::DURATION_SECS;
        }
    }

    fn activate(&mut self, path: &str) {
        match path {
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

fn waveform() -> Vec<WaveBucket> {
    (0_u16..160)
        .map(|index| WaveBucket {
            low: 0.25 + f32::from((index * 17) % 70) / 100.0,
            mid: 0.18 + f32::from((index * 29 + 11) % 65) / 100.0,
            high: 0.12 + f32::from((index * 41 + 23) % 55) / 100.0,
        })
        .collect()
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
    registry
}
