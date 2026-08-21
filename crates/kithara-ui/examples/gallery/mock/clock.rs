use kithara_ui::render::ReadValue;

const SOURCES: [Source; 8] = [
    Source::new("auto", "AUTO", "124.00", "FOLLOW", "+0.0 ms", "0.00%"),
    Source::new("clock", "CLK", "124.00", "MASTER", "0.0 ms", "0.00%"),
    Source::new("a", "A", "124.00", "DECK", "+1.4 ms", "+0.02%"),
    Source::new("b", "B", "123.98", "DECK", "-2.1 ms", "-0.01%"),
    Source::new("c", "C", "126.00", "DECK", "+0.8 ms", "+1.61%"),
    Source::new("d", "D", "122.00", "DECK", "-0.6 ms", "-1.61%"),
    Source::new("link", "LINK", "124.00", "ABLETON", "+0.2 ms", "0.00%"),
    Source::new("midi", "MIDI", "124.01", "CLOCK", "-0.3 ms", "+0.01%"),
];

struct Source {
    id: &'static str,
    name: &'static str,
    tempo: &'static str,
    mode: &'static str,
    pulse: &'static str,
    stretch: &'static str,
}

impl Source {
    const fn new(
        id: &'static str,
        name: &'static str,
        tempo: &'static str,
        mode: &'static str,
        pulse: &'static str,
        stretch: &'static str,
    ) -> Self {
        Self {
            id,
            name,
            tempo,
            mode,
            pulse,
            stretch,
        }
    }
}

pub(super) struct ClockState {
    bpm_label: String,
    source: usize,
    family_step: bool,
    open: bool,
    quantize: bool,
    snap: bool,
    click: bool,
    key_locked: bool,
    link: bool,
    midi_send: bool,
    bpm: f64,
}

impl Default for ClockState {
    fn default() -> Self {
        let mut state = Self {
            bpm_label: String::new(),
            source: 0,
            family_step: true,
            open: true,
            quantize: true,
            snap: true,
            click: false,
            key_locked: true,
            link: true,
            midi_send: false,
            bpm: 124.0,
        };
        state.rebuild();
        state
    }
}

impl ClockState {
    pub(super) const fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub(super) fn activate(&mut self, path: &str) -> bool {
        if path.contains("key-lock/") {
            match path.rsplit('/').next() {
                Some("toggle") => self.key_locked = !self.key_locked,
                Some("down" | "up") => {}
                _ => return false,
            }
            return true;
        }
        if !path.contains("clock") {
            return false;
        }
        if let Some(source) = SOURCES
            .iter()
            .position(|source| path.contains(&format!("source-{}/select", source.id)))
        {
            self.source = source;
            return true;
        }
        match path.rsplit('/').next() {
            Some("toggle") => self.open = !self.open,
            Some("pop" | "close") => self.open = false,
            Some("up" | "nudge_up") => self.step(0.01),
            Some("down" | "nudge_down") => self.step(-0.01),
            Some("step") => self.family_step = true,
            Some("leap") => self.family_step = false,
            Some("quantize") => self.quantize = !self.quantize,
            Some("snap") => self.snap = !self.snap,
            Some("click") => self.click = !self.click,
            Some("link-toggle") => self.link = !self.link,
            Some("midi-send") => self.midi_send = !self.midi_send,
            Some("tap") => self.set_bpm(125.0),
            Some("half") => self.set_bpm(self.bpm / 2.0),
            Some("double") => self.set_bpm(self.bpm * 2.0),
            Some("reset") => self.set_bpm(124.0),
            _ => return false,
        }
        true
    }

    pub(super) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (id, scope) = endpoint.split_once('@').unwrap_or((endpoint, ""));
        let source = scope_value(scope, "source")
            .and_then(|id| SOURCES.iter().find(|source| source.id == id));
        let value = match id {
            "clock.open" => ReadValue::Bool(self.open),
            "clock.bpm" => ReadValue::Text(&self.bpm_label),
            "clock.source" => ReadValue::Text(SOURCES[self.source].name),
            "clock.warning" => ReadValue::Text("ALL SOURCES STABLE"),
            "clock.family.step" => ReadValue::Bool(self.family_step),
            "clock.family.leap" => ReadValue::Bool(!self.family_step),
            "clock.limit" => ReadValue::Scalar(8.0),
            "clock.tolerance" => ReadValue::Scalar(0.02),
            "clock.grid.quantize" => ReadValue::Bool(self.quantize),
            "clock.grid.division" => ReadValue::Text("1/16"),
            "clock.grid.snap" => ReadValue::Bool(self.snap),
            "clock.grid.click" => ReadValue::Bool(self.click),
            "clock.link.enabled" => ReadValue::Bool(self.link),
            "clock.link.peers" => ReadValue::Text("2 PEERS"),
            "clock.midi.input" => ReadValue::Text("TR-8S"),
            "clock.midi.output" => ReadValue::Text("ALL PORTS"),
            "clock.midi.send" => ReadValue::Bool(self.midi_send),
            "clock.source.active" => ReadValue::Bool(source?.id == SOURCES[self.source].id),
            "clock.source.name" => ReadValue::Text(source?.name),
            "clock.source.tempo" => ReadValue::Text(source?.tempo),
            "clock.source.mode" => ReadValue::Text(source?.mode),
            "clock.source.pulse" => ReadValue::Text(source?.pulse),
            "clock.source.stretch" => ReadValue::Text(source?.stretch),
            "clock.source.synced" => ReadValue::Bool(source?.id != "c" && source?.id != "d"),
            "deck.key.locked" => ReadValue::Bool(self.key_locked),
            _ => return None,
        };
        Some(value)
    }

    pub(super) fn step(&mut self, steps: f64) {
        self.set_bpm(self.bpm + steps);
    }

    fn set_bpm(&mut self, bpm: f64) {
        self.bpm = bpm.clamp(40.0, 250.0);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.bpm_label = format!("{:.2}", self.bpm);
    }
}

fn scope_value<'a>(scope: &'a str, name: &str) -> Option<&'a str> {
    scope
        .split(',')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
}
