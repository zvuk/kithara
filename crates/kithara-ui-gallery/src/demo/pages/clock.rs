use kithara_ui::render::{ReadValue, Scope, WriteValue};

mod consts {
    use super::Source;

    pub(super) const SOURCES: [Source; 8] = [
        Source::new("auto", "AUTO", "124.00", "FOLLOW", "+0.0 ms", "0.00%"),
        Source::new("clock", "CLK", "124.00", "MASTER", "0.0 ms", "0.00%"),
        Source::new("a", "A", "124.00", "DECK", "+1.4 ms", "+0.02%"),
        Source::new("b", "B", "123.98", "DECK", "-2.1 ms", "-0.01%"),
        Source::new("c", "C", "126.00", "DECK", "+0.8 ms", "+1.61%"),
        Source::new("d", "D", "122.00", "DECK", "-0.6 ms", "-1.61%"),
        Source::new("link", "LINK", "124.00", "ABLETON", "+0.2 ms", "0.00%"),
        Source::new("midi", "MIDI", "124.01", "CLOCK", "-0.3 ms", "+0.01%"),
    ];
}

struct Source {
    id: &'static str,
    mode: &'static str,
    name: &'static str,
    pulse: &'static str,
    stretch: &'static str,
    tempo: &'static str,
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
            mode,
            name,
            pulse,
            stretch,
            tempo,
        }
    }
}

pub(crate) struct ClockState {
    bpm_label: String,
    click: bool,
    family_step: bool,
    key_locked: bool,
    transpose: i8,
    transpose_label: String,
    link: bool,
    midi_send: bool,
    quantize: bool,
    snap: bool,
    bpm: f64,
    source: usize,
}

impl Default for ClockState {
    fn default() -> Self {
        let mut state = Self {
            bpm_label: String::new(),
            source: 0,
            family_step: true,
            quantize: true,
            snap: true,
            click: false,
            key_locked: true,
            transpose: 0,
            transpose_label: String::new(),
            link: true,
            midi_send: false,
            bpm: 124.0,
        };
        state.rebuild();
        state
    }
}

impl ClockState {
    pub(crate) fn write(&mut self, id: &str, scope: Scope<'_>, value: &WriteValue) {
        match (id, value) {
            ("clock.source.select", WriteValue::Trigger) => {
                if let Some(source) = scope
                    .get("source")
                    .and_then(|id| consts::SOURCES.iter().position(|source| source.id == id))
                {
                    self.source = source;
                }
            }
            ("clock.tempo", WriteValue::Step(steps)) => self.step(f64::from(*steps) * 0.01),
            ("clock.nudge_up", WriteValue::Trigger) => self.step(0.01),
            ("clock.nudge_down", WriteValue::Trigger) => self.step(-0.01),
            ("clock.family.step", WriteValue::Trigger) => self.family_step = true,
            ("clock.family.leap", WriteValue::Trigger) => self.family_step = false,
            ("clock.grid.toggle_quantize", WriteValue::Trigger) => self.quantize = !self.quantize,
            ("clock.grid.toggle_snap", WriteValue::Trigger) => self.snap = !self.snap,
            ("clock.grid.toggle_click", WriteValue::Trigger) => self.click = !self.click,
            ("clock.link.toggle", WriteValue::Trigger) => self.link = !self.link,
            ("clock.midi.toggle_send", WriteValue::Trigger) => self.midi_send = !self.midi_send,
            ("clock.tap", WriteValue::Trigger) => self.set_bpm(125.0),
            ("clock.half", WriteValue::Trigger) => self.set_bpm(self.bpm / 2.0),
            ("clock.double", WriteValue::Trigger) => self.set_bpm(self.bpm * 2.0),
            ("clock.reset", WriteValue::Trigger) => self.set_bpm(124.0),
            ("deck.key.toggle_lock", WriteValue::Trigger) => self.key_locked = !self.key_locked,
            ("deck.key.transpose_up", WriteValue::Trigger) => self.transpose_to(self.transpose + 1),
            ("deck.key.transpose_down", WriteValue::Trigger) => {
                self.transpose_to(self.transpose - 1);
            }
            ("deck.key.transpose_reset", WriteValue::Trigger) => self.transpose_to(0),
            _ => {}
        }
    }

    pub(crate) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (id, scope) = endpoint.split_once('@').unwrap_or((endpoint, ""));
        let source = scope_value(scope, "source")
            .and_then(|id| consts::SOURCES.iter().find(|source| source.id == id));
        let value = match id {
            "clock.bpm" => ReadValue::Text(&self.bpm_label),
            "clock.source" => ReadValue::Text(consts::SOURCES[self.source].name),
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
            "clock.source.active" => ReadValue::Bool(source?.id == consts::SOURCES[self.source].id),
            "clock.source.name" => ReadValue::Text(source?.name),
            "clock.source.tempo" => ReadValue::Text(source?.tempo),
            "clock.source.mode" => ReadValue::Text(source?.mode),
            "clock.source.pulse" => ReadValue::Text(source?.pulse),
            "clock.source.stretch" => ReadValue::Text(source?.stretch),
            "clock.source.synced" => ReadValue::Bool(source?.id != "c" && source?.id != "d"),
            "deck.key.locked" => ReadValue::Bool(self.key_locked),
            "deck.key.transpose" => ReadValue::Text(&self.transpose_label),
            _ => return None,
        };
        Some(value)
    }

    fn rebuild(&mut self) {
        self.bpm_label = format!("{:.2}", self.bpm);
    }

    fn set_bpm(&mut self, bpm: f64) {
        self.bpm = bpm.clamp(40.0, 250.0);
        self.rebuild();
    }

    fn step(&mut self, steps: f64) {
        self.set_bpm(self.bpm + steps);
    }

    fn transpose_to(&mut self, semitones: i8) {
        const MINUS_SIGN: char = '\u{2212}';

        self.transpose = semitones.clamp(-12, 12);
        self.transpose_label = match self.transpose {
            0 => String::new(),
            up if up > 0 => format!("+{up}"),
            down => format!("{MINUS_SIGN}{}", down.unsigned_abs()),
        };
    }
}

fn scope_value<'a>(scope: &'a str, name: &str) -> Option<&'a str> {
    scope
        .split(',')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
}
