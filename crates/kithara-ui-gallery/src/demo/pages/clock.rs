use kithara_ui::render::{ReadValue, Scope, WriteValue};

mod consts {
    use kithara_key::{Mode, MusicalKey, PitchClass};

    use super::Source;

    pub(super) const TRACK_KEY: MusicalKey = MusicalKey {
        mode: Mode::Minor,
        tonic: PitchClass::FSharp,
    };

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
    transpose_label: String,
    bpm: f64,
    source: usize,
    click: bool,
    family_step: bool,
    key_locked: bool,
    link: bool,
    midi_send: bool,
    quantize: bool,
    snap: bool,
    transpose: i8,
}

impl Default for ClockState {
    fn default() -> Self {
        let mut state = Self {
            bpm_label: String::new(),
            transpose_label: String::new(),
            bpm: 124.0,
            source: 0,
            click: false,
            family_step: true,
            key_locked: true,
            link: true,
            midi_send: false,
            quantize: true,
            snap: true,
            transpose: 0,
        };
        state.rebuild();
        state
    }
}

impl ClockState {
    pub(crate) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (id, scope) = endpoint.split_once('@').unwrap_or((endpoint, ""));
        self.read_clock(id)
            .or_else(|| self.read_grid(id))
            .or_else(|| self.read_sync(id))
            .or_else(|| self.read_source(id, scope))
            .or_else(|| self.read_deck_key(id))
    }

    pub(crate) fn write(&mut self, id: &str, scope: Scope<'_>, value: &WriteValue) {
        if let ("clock.tempo", WriteValue::Step(steps)) = (id, value) {
            self.step(f64::from(*steps) * 0.01);
            return;
        }
        let WriteValue::Trigger = value else {
            return;
        };
        if id == "clock.source.select" {
            self.select_source(scope);
        } else if !self.write_clock(id) && !self.write_grid_and_sync(id) {
            self.write_deck_key(id);
        }
    }

    fn read_clock(&self, id: &str) -> Option<ReadValue<'_>> {
        let value = match id {
            "clock.bpm" => ReadValue::Text(&self.bpm_label),
            "clock.source" => ReadValue::Text(consts::SOURCES[self.source].name),
            "clock.warning" => ReadValue::Text("ALL SOURCES STABLE"),
            "clock.family.step" => ReadValue::Bool(self.family_step),
            "clock.family.leap" => ReadValue::Bool(!self.family_step),
            "clock.limit" => ReadValue::Scalar(8.0),
            "clock.tolerance" => ReadValue::Scalar(0.02),
            _ => return None,
        };
        Some(value)
    }

    fn read_deck_key(&self, id: &str) -> Option<ReadValue<'_>> {
        let value = match id {
            "deck.track.key" => ReadValue::Text(consts::TRACK_KEY.open_key()),
            "deck.track.key_color" => ReadValue::Color(
                kithara_ui::builtin::MINOR_KEY_COLORS[consts::TRACK_KEY.open_key_index()],
            ),
            "deck.key.sounding" => {
                ReadValue::Text(consts::TRACK_KEY.transposed(self.transpose).open_key())
            }
            "deck.key.color" => ReadValue::Color(
                kithara_ui::builtin::MINOR_KEY_COLORS[consts::TRACK_KEY
                    .transposed(self.transpose)
                    .open_key_index()],
            ),
            "deck.key.sounding_hidden" => ReadValue::Bool(false),
            "deck.key.transpose_hidden" => ReadValue::Bool(self.transpose == 0),
            "deck.key.locked" => ReadValue::Bool(self.key_locked),
            "deck.key.transpose" => ReadValue::Text(&self.transpose_label),
            _ => return None,
        };
        Some(value)
    }

    fn read_grid(&self, id: &str) -> Option<ReadValue<'_>> {
        let value = match id {
            "clock.grid.quantize" => ReadValue::Bool(self.quantize),
            "clock.grid.division" => ReadValue::Text("1/16"),
            "clock.grid.snap" => ReadValue::Bool(self.snap),
            "clock.grid.click" => ReadValue::Bool(self.click),
            _ => return None,
        };
        Some(value)
    }

    fn read_source(&self, id: &str, scope: &str) -> Option<ReadValue<'_>> {
        let source = find_source(scope)?;
        let value = match id {
            "clock.source.active" => ReadValue::Bool(source.id == consts::SOURCES[self.source].id),
            "clock.source.name" => ReadValue::Text(source.name),
            "clock.source.tempo" => ReadValue::Text(source.tempo),
            "clock.source.mode" => ReadValue::Text(source.mode),
            "clock.source.pulse" => ReadValue::Text(source.pulse),
            "clock.source.stretch" => ReadValue::Text(source.stretch),
            "clock.source.synced" => ReadValue::Bool(!matches!(source.id, "c" | "d")),
            _ => return None,
        };
        Some(value)
    }

    fn read_sync(&self, id: &str) -> Option<ReadValue<'_>> {
        let value = match id {
            "clock.link.enabled" => ReadValue::Bool(self.link),
            "clock.link.peers" => ReadValue::Text("2 PEERS"),
            "clock.midi.input" => ReadValue::Text("TR-8S"),
            "clock.midi.output" => ReadValue::Text("ALL PORTS"),
            "clock.midi.send" => ReadValue::Bool(self.midi_send),
            _ => return None,
        };
        Some(value)
    }

    fn rebuild(&mut self) {
        self.bpm_label = format!("{:.2}", self.bpm);
    }

    fn select_source(&mut self, scope: Scope<'_>) {
        if let Some(source) = scope
            .get("source")
            .and_then(|id| consts::SOURCES.iter().position(|source| source.id == id))
        {
            self.source = source;
        }
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

    fn write_clock(&mut self, id: &str) -> bool {
        match id {
            "clock.nudge_up" => self.step(0.01),
            "clock.nudge_down" => self.step(-0.01),
            "clock.family.step" => self.family_step = true,
            "clock.family.leap" => self.family_step = false,
            "clock.tap" => self.set_bpm(125.0),
            "clock.half" => self.set_bpm(self.bpm / 2.0),
            "clock.double" => self.set_bpm(self.bpm * 2.0),
            "clock.reset" => self.set_bpm(124.0),
            _ => return false,
        }
        true
    }

    fn write_deck_key(&mut self, id: &str) {
        match id {
            "deck.key.toggle_lock" => self.key_locked = !self.key_locked,
            "deck.key.transpose_up" => self.transpose_to(self.transpose + 1),
            "deck.key.transpose_down" => self.transpose_to(self.transpose - 1),
            "deck.key.transpose_reset" => self.transpose_to(0),
            _ => {}
        }
    }

    fn write_grid_and_sync(&mut self, id: &str) -> bool {
        match id {
            "clock.grid.toggle_quantize" => self.quantize = !self.quantize,
            "clock.grid.toggle_snap" => self.snap = !self.snap,
            "clock.grid.toggle_click" => self.click = !self.click,
            "clock.link.toggle" => self.link = !self.link,
            "clock.midi.toggle_send" => self.midi_send = !self.midi_send,
            _ => return false,
        }
        true
    }
}

fn find_source(scope: &str) -> Option<&'static Source> {
    let id = scope_value(scope, "source")?;
    consts::SOURCES.iter().find(|source| source.id == id)
}

fn scope_value<'a>(scope: &'a str, name: &str) -> Option<&'a str> {
    scope
        .split(',')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
}
