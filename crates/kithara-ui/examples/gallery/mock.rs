use std::collections::BTreeMap;

use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ControlAction, ReadValue, Reads, StereoLevels},
};
use num_traits::cast::AsPrimitive;

struct Consts;

impl Consts {
    const ARTIST: &str = "teo_van_bo";
    const BPM: &str = "70.00";
    const KEY: &str = "4m";
    const REMAIN: &str = "−04:17";
    const TRACK_TITLE: &str = "MoonShine_Секрет";
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Tab {
    Atoms,
    Buttons,
    Faders,
}

impl Tab {
    pub(super) const ALL: [Self; 3] = [Self::Atoms, Self::Buttons, Self::Faders];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Atoms => "gallery-atoms.klayout.ron",
            Self::Buttons => "gallery-buttons.klayout.ron",
            Self::Faders => "gallery-faders.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Atoms => 0,
            Self::Buttons => 1,
            Self::Faders => 2,
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
    toggle_off: bool,
    toggle_on: bool,
    volume: f64,
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
            knob: 0.62,
            levels_volume: 0.7,
            toggle_off: false,
            toggle_on: true,
            volume: 0.7,
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
        } else if path.starts_with("faders/") {
            self.volume = value;
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
            "mock.track.title" => ReadValue::Text(Consts::TRACK_TITLE),
            "mock.track.artist" => ReadValue::Text(Consts::ARTIST),
            "mock.bpm" => ReadValue::Text(Consts::BPM),
            "mock.key" => ReadValue::Text(Consts::KEY),
            "mock.remain" => ReadValue::Text(Consts::REMAIN),
            "mock.knob" => ReadValue::Scalar(self.knob),
            "mock.levels" => ReadValue::Stereo(StereoLevels {
                l: 0.66,
                r: 0.52,
                volume: self.levels_volume.as_(),
            }),
            "mock.volume" => ReadValue::Scalar(self.volume),
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

#[derive(Default)]
struct MockRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl MockRegistry {
    fn insert(&mut self, id: &str, value: ValueKind) {
        self.endpoints.insert(
            (EndpointCategory::Model, EndpointId(id.to_owned())),
            EndpointDesc::new(value),
        );
    }
}

impl EndpointRegistry for MockRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

pub(super) fn registry() -> impl EndpointRegistry {
    let mut registry = MockRegistry::default();
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
        registry.insert(id, ValueKind::Text);
    }
    for id in [
        "gallery.tab.atoms",
        "gallery.tab.buttons",
        "gallery.tab.faders",
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
        registry.insert(id, ValueKind::Bool);
    }
    for id in ["mock.knob", "mock.volume"] {
        registry.insert(id, ValueKind::Scalar);
    }
    registry.insert("mock.levels", ValueKind::Stereo);
    registry
}
