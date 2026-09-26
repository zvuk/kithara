use std::collections::{BTreeMap, BTreeSet};

use kithara_ui::{
    builtin,
    render::{
        ControlAction, ReadValue, Reads, Skin, StereoLevels, TreeRow, WaveBucket, WaveformView,
    },
};
use num_traits::cast::AsPrimitive;

use super::{
    consts,
    data::CATALOG,
    pages::{
        clock::ClockState,
        menu::{ContextState, MenuState},
        mixer::MixerState,
        pivot::PivotState,
        scene::SceneState,
        stress::StressState,
        transport::DeckTransport,
    },
    quality::QualityState,
};
use crate::sections::{self, Page};

/// The face families the assets page sets its specimen in, named as the
/// document writes them. The page offers one switch per name and the endpoints
/// behind them are built from this list, so a family is added by writing it
/// here and in the document.
pub(crate) const FONT_FAMILIES: [&str; 3] = ["display", "sans", "mono"];

/// What this application moves on the page it is showing.
///
/// A document declares its own motion and a host reads that declaration off
/// the compiled page. This is the other kind, which no document can declare: a
/// reading the application hands over afresh every frame. The window has to
/// know before it ticks, and the tick has to know what to move, so both read
/// this one answer rather than each keeping a list of pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Feed {
    /// The numbers the stress page reports about drawing itself.
    Bench,
    /// The levels and spectra the visualiser page draws.
    Vis,
    /// How far along each object on the objects page is.
    Phase,
    /// Plain seconds, which is all the pages that place their own objects get.
    Clock,
}

impl Feed {
    fn of(tab: Page) -> Option<Self> {
        match tab {
            "stress" => Some(Self::Bench),
            "vis" => Some(Self::Vis),
            "objects" => Some(Self::Phase),
            "motion" | "sprites" | "lottie" | "scene" => Some(Self::Clock),
            _ => None,
        }
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct DemoReads {
    table_widths: BTreeMap<String, f64>,
    collapsed: BTreeSet<String>,
    clock: ClockState,
    context: ContextState,
    transport: DeckTransport,
    menu: MenuState,
    mixer: MixerState,
    /// The page on screen, which the screen's own state owns and this model
    /// is told after every turn: a page with a feed of its own is fed only
    /// while it is the page shown.
    showing: Page,
    pivot: PivotState,
    quality: QualityState,
    scene: SceneState,
    stress: StressState,
    #[field(set, vis = "pub(crate)")]
    library_query: String,
    tree_expanded: Vec<bool>,
    tree_rows: Vec<TreeRow<'static>>,
    tree_visible_indices: Vec<usize>,
    wave_beats: Vec<f32>,
    wave_downbeats: Vec<f32>,
    waveform: Vec<WaveBucket>,
    table_columns: [bool; 9],
    vis_levels: [f32; 2],
    knobs: [f64; 4],
    button_cue: bool,
    button_play: bool,
    button_sync: bool,
    checkbox_off: bool,
    checkbox_on: bool,
    chip_active: bool,
    chip_inactive: bool,
    toggle_off: bool,
    toggle_on: bool,
    lottie_scrub: f32,
    motion_clock: f32,
    motion_phase: f32,
    sprite_scrub: f32,
    vis_phase: f32,
    levels_volume: f64,
    segmented_index: f64,
    vis_time_secs: f64,
    volume: f64,
    vis_rng: u32,
    /// Which family the assets page sets its specimen in, as an index into
    /// [`FONT_FAMILIES`]. It sits beside the skin for the same reason: the
    /// choice outlives the page it was made on.
    active_font: usize,
    /// Which shipped skin the gallery wears, as an index into
    /// [`builtin::skins`]. It lives beside the page rather than on it, so a
    /// skin chosen here outlives every page turned afterwards.
    #[field(get, vis = "pub(crate)", copy)]
    active_skin: usize,
    library_scope: usize,
    table_preset: usize,
    tree_selected: usize,
    vis_preset: usize,
}

impl Default for DemoReads {
    fn default() -> Self {
        let (wave_beats, wave_downbeats) = beat_grid();
        let tree_expanded = CATALOG
            .tree
            .iter()
            .map(|row| row.expanded.unwrap_or(false))
            .collect();
        let tree_selected = CATALOG
            .tree
            .iter()
            .position(|row| row.selected)
            .unwrap_or_default();
        let mut reads = Self {
            wave_beats,
            wave_downbeats,
            tree_expanded,
            tree_selected,
            showing: sections::first(),
            active_skin: 0,
            active_font: 0,
            button_cue: false,
            button_play: false,
            button_sync: true,
            checkbox_off: false,
            checkbox_on: true,
            chip_active: true,
            chip_inactive: false,
            collapsed: BTreeSet::new(),
            clock: ClockState::default(),
            context: ContextState::default(),
            knobs: [0.35, 0.5, 0.65, 0.8],
            levels_volume: 0.7,
            library_query: String::new(),
            library_scope: 0,
            menu: MenuState::default(),
            pivot: PivotState::default(),
            mixer: MixerState::default(),
            quality: QualityState::default(),
            scene: SceneState::default(),
            segmented_index: 2.0,
            stress: StressState::default(),
            toggle_off: false,
            toggle_on: true,
            volume: 0.7,
            waveform: waveform(),
            transport: DeckTransport::new(
                consts::BPM_VALUE,
                consts::CUES,
                consts::DURATION_SECS,
                consts::LOOP_REGION,
                consts::POSITION_SECS,
                consts::ZOOM,
            ),
            table_columns: consts::TABLE_QUEUE,
            table_preset: consts::TABLE_QUEUE_PRESET,
            table_widths: BTreeMap::new(),
            tree_rows: Vec::with_capacity(CATALOG.tree.len()),
            tree_visible_indices: Vec::with_capacity(CATALOG.tree.len()),
            motion_phase: consts::MOTION_START,
            motion_clock: consts::MOTION_CLOCK_START,
            sprite_scrub: consts::SPRITE_SCRUB_START,
            lottie_scrub: consts::LOTTIE_SCRUB_START,
            vis_levels: [0.66, 0.52],
            vis_phase: 0.0,
            vis_preset: 0,
            vis_time_secs: 0.0,
            vis_rng: 0x8a17_4c3d,
        };
        reads.rebuild_tree();
        reads
    }
}

impl DemoReads {
    fn activate(&mut self, path: &str) {
        if self.clock.activate(path) {
            return;
        }
        if self.pivot.activate(path) {
            return;
        }
        if self.menu.activate(path) {
            return;
        }
        if self.context.activate(path) {
            return;
        }
        if self.scene.activate(path) {
            return;
        }
        if self.quality.activate(path) {
            return;
        }
        if self.mixer.activate(path) {
            return;
        }
        if self.transport.activate(path) {
            return;
        }
        match path {
            "atoms/toggles/toggle-on" | "cells/toggle-on" => self.toggle_on = !self.toggle_on,
            "atoms/toggles/toggle-off" | "cells/toggle-off" => self.toggle_off = !self.toggle_off,
            "atoms/toggles/checkbox-on" | "cells/checkbox-on" => {
                self.checkbox_on = !self.checkbox_on;
            }
            "atoms/toggles/checkbox-off" | "cells/checkbox-off" => {
                self.checkbox_off = !self.checkbox_off;
            }
            "atoms/chips/active" => self.chip_active = !self.chip_active,
            "atoms/chips/inactive" => self.chip_inactive = !self.chip_inactive,
            "buttons/play" => self.button_play = !self.button_play,
            "buttons/cue" => self.button_cue = !self.button_cue,
            "buttons/sync" => self.button_sync = !self.button_sync,
            "table/reset-columns" => self.reset_table_columns(),
            "vis/next" => self.vis_preset = (self.vis_preset + 1) % CATALOG.vis_presets.len(),
            "vis/previous" => {
                self.vis_preset =
                    (self.vis_preset + CATALOG.vis_presets.len() - 1) % CATALOG.vis_presets.len();
            }
            path if path.starts_with("table/column-") => {
                self.toggle_table_column(&path["table/column-".len()..]);
            }
            path if let Some(skin) = path
                .strip_prefix("skins/")
                .and_then(|rest| rest.strip_suffix("/item")) =>
            {
                self.select_skin(skin);
            }
            path if let Some(family) = path
                .strip_prefix("assets/")
                .and_then(|rest| rest.strip_suffix("/item")) =>
            {
                self.select_font(family);
            }
            path if path.ends_with("/transport/sync") => {
                self.button_sync = !self.button_sync;
            }
            path if path.ends_with("/play") => self.transport.toggle_play(),
            _ => {}
        }
    }

    pub(crate) fn apply(&mut self, path: &str, action: &ControlAction) {
        match action {
            ControlAction::SetScalar(value) => self.set_scalar(path, *value),
            ControlAction::Activate => self.activate(path),
            ControlAction::SecondaryActivate => self.context.secondary(path),
            ControlAction::SelectIndex(index) => self.select_index(path, *index),
            ControlAction::Place(at) => {
                self.scene.place(path, *at);
            }
            ControlAction::StepScalar(steps) if path.contains("clock") => {
                self.clock.step(f64::from(*steps) * 0.01);
            }
            _ => {}
        }
    }

    /// Whether the application moves a reading on the page it is showing.
    pub(crate) fn feeds(&self) -> bool {
        Feed::of(self.showing).is_some()
    }

    fn rebuild_tree(&mut self) {
        self.tree_rows.clear();
        self.tree_visible_indices.clear();
        let mut ancestors = Vec::new();
        for (index, base) in CATALOG.tree.iter().copied().enumerate() {
            let depth = usize::from(base.depth);
            ancestors.truncate(depth);
            let visible = ancestors.iter().all(|expanded| *expanded);
            if visible {
                self.tree_rows.push(TreeRow {
                    expanded: base.expanded.map(|_| self.tree_expanded[index]),
                    selected: index == self.tree_selected,
                    ..base
                });
                self.tree_visible_indices.push(index);
            }
            if base.expanded.is_some() {
                ancestors.push(self.tree_expanded[index]);
            }
        }
    }

    fn reset_table_columns(&mut self) {
        self.set_table_preset(self.table_preset);
    }

    /// Sets the specimen in the family of that name. A name no shipped family
    /// answers to leaves the specimen in the one it is set in.
    fn select_font(&mut self, family: &str) {
        if let Some(index) = FONT_FAMILIES.iter().position(|named| *named == family) {
            self.active_font = index;
        }
    }

    fn select_index(&mut self, path: &str, index: usize) {
        if path == "cells/beat" {
            self.segmented_index = index.as_();
        } else if path == "table/column-preset" {
            self.set_table_preset(index);
        } else if path == "library2/context" {
            self.library_scope = index;
        } else if matches!(path, "tree/browser" | "library2/browser") {
            self.select_tree_row(index);
        } else if path == "vis/shader" && index < CATALOG.vis_presets.len() {
            self.vis_preset = index;
        }
    }

    /// Turns to the shipped skin of that name. A name no shipped skin answers
    /// to leaves the gallery in the one it is wearing.
    fn select_skin(&mut self, id: &str) {
        if let Some(index) = builtin::skins().iter().position(|skin| skin.id() == id) {
            self.active_skin = index;
        }
    }

    fn select_tree_row(&mut self, index: usize) {
        let Some(base_index) = self.tree_visible_indices.get(index).copied() else {
            return;
        };
        let row = CATALOG.tree[base_index];
        if row.muted {
            return;
        }
        if row.expanded.is_some() {
            self.tree_expanded[base_index] = !self.tree_expanded[base_index];
        } else {
            self.tree_selected = base_index;
        }
        self.rebuild_tree();
    }

    fn set_scalar(&mut self, path: &str, value: f64) {
        if self.pivot.set_scalar(path, value) {
            return;
        }
        if self.mixer.set_scalar(path, value) {
            return;
        }
        if self.stress.set_scalar(path, value) {
            return;
        }
        if let Some((_, name)) = path.rsplit_once("/width/") {
            self.set_table_width(name, value);
            return;
        }
        let value = value.clamp(0.0, 1.0);
        if path == "sprites/scrub" {
            self.sprite_scrub = value.as_();
        } else if path == "lottie/scrub" {
            self.lottie_scrub = value.as_();
        } else if path.ends_with("/loop_start") {
            self.transport.set_loop_start(value);
        } else if path.ends_with("/loop_end") {
            self.transport.set_loop_end(value);
        } else if path.ends_with("/zoom") {
            self.transport.set_zoom(value);
        } else if let Some(index) = match path {
            "atoms/knobs/size-26" => Some(0),
            "atoms/knobs/size-28" => Some(1),
            "atoms/knobs/size-34" => Some(2),
            "atoms/knobs/size-38" => Some(3),
            _ => None,
        } {
            self.knobs[index] = value;
        } else if path.starts_with("atoms/meters/") {
            self.levels_volume = value;
        } else if path.starts_with("faders/") || path.ends_with("/volume") {
            self.volume = value;
        } else if path.ends_with("/wave") {
            self.transport.seek_normalized(value);
        }
    }

    fn set_table_preset(&mut self, index: usize) {
        let Some(columns) = [
            consts::TABLE_LIBRARY,
            consts::TABLE_QUEUE,
            consts::TABLE_MICRO,
        ]
        .get(index)
        .copied() else {
            return;
        };
        self.table_preset = index;
        self.table_columns = columns;
    }

    fn set_table_width(&mut self, name: &str, value: f64) {
        if !consts::table_columns()
            .iter()
            .any(|column| column.id() == name)
        {
            return;
        }
        if value.is_finite() {
            let minimum = f64::from(builtin::skin().table.min_column_width);
            self.table_widths
                .insert(name.to_owned(), value.max(minimum));
        }
    }

    /// Rebuilds the stress page's waveforms at a different bucket count, which
    /// is the one weight of that page a measurement can vary. The gallery shows
    /// the page at its own count; only a harness sweeps it.
    #[cfg(test)]
    pub(crate) fn set_wave_buckets(&mut self, buckets: u16) {
        self.stress = StressState::new(buckets);
    }

    fn shell(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let value = match endpoint {
            endpoint if let Some(skin) = endpoint.strip_prefix("gallery.skin.") => {
                builtin::skins()[self.active_skin].id() == skin
            }
            endpoint if let Some(rest) = endpoint.strip_prefix("gallery.font.") => {
                let active = FONT_FAMILIES[self.active_font];
                rest.strip_suffix(".hidden")
                    .map_or_else(|| active == rest, |family| active != family)
            }
            _ => return None,
        };
        Some(ReadValue::Bool(value))
    }

    /// Told which page the screen now stands at. A page arriving is a page
    /// opening, so the feed behind it starts where the page does.
    pub(crate) fn show(&mut self, page: Page) {
        if self.showing != page {
            self.stress.reset_clock();
        }
        self.showing = page;
    }

    /// The skin the gallery is dressed in, which every host asks for and no
    /// page turn touches.
    pub(crate) fn skin(&self) -> &'static Skin {
        &builtin::skins()[self.active_skin]
    }

    pub(crate) fn tick(&mut self) {
        match Feed::of(self.showing) {
            Some(Feed::Bench) => self.stress.tick(),
            Some(Feed::Vis) => self.tick_vis(),
            Some(Feed::Phase) => self.tick_phase(),
            Some(Feed::Clock) => self.tick_clock(),
            None => {}
        }
    }

    /// Plain seconds, which is all the motion page's application knows: how far
    /// along that puts each object is the document's business, not its own.
    fn tick_clock(&mut self) {
        self.motion_clock =
            (self.motion_clock + consts::MOTION_TICK_SECS) % consts::MOTION_CLOCK_PERIOD;
    }

    /// One sawtooth from 0 to 1, which is every track on the objects page: an
    /// application that already knows how far along each object is hands the
    /// number over and the document spends it.
    fn tick_phase(&mut self) {
        self.motion_phase = (self.motion_phase + consts::MOTION_STEP).fract();
    }

    fn tick_vis(&mut self) {
        self.vis_time_secs += consts::VIS_TICK_SECS;
        self.vis_phase += 0.17;
        self.vis_rng = self
            .vis_rng
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let left_noise: f32 = (self.vis_rng >> 16).as_();
        let right_noise: f32 = (self.vis_rng & 0xffff).as_();
        let scale = f32::from(u16::MAX);
        self.vis_levels = [
            (left_noise / scale)
                .mul_add(0.14, self.vis_phase.sin().abs().mul_add(0.32, 0.42))
                .clamp(0.0, 1.0),
            (right_noise / scale)
                .mul_add(
                    0.12,
                    (self.vis_phase * 1.31).sin().abs().mul_add(0.29, 0.38),
                )
                .clamp(0.0, 1.0),
        ];
    }

    pub(crate) fn toggle_module(&mut self, module: String) {
        if !self.collapsed.remove(&module) {
            self.collapsed.insert(module);
        }
    }

    fn toggle_table_column(&mut self, name: &str) {
        let Some(index) = consts::table_columns()
            .iter()
            .position(|column| column.id() == name)
        else {
            return;
        };
        self.table_columns[index] = !self.table_columns[index];
    }
}

impl Reads for DemoReads {
    /// Answers scope-specific fields (menu, context, quality, clock) before generic ones, since
    /// those axes are genuinely per-window, per-module, or per-row. The gallery hosts a single
    /// virtual deck, so every `@scope` suffix resolves to the same state and is dropped.
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        if let Some(value) = self
            .menu
            .get(endpoint)
            .or_else(|| self.context.get(endpoint))
            .or_else(|| self.quality.get(endpoint))
            .or_else(|| self.clock.get(endpoint))
            .or_else(|| self.pivot.get(endpoint))
            .or_else(|| self.scene.get(endpoint))
        {
            return Some(value);
        }
        let endpoint = endpoint.split_once('@').map_or(endpoint, |(base, _)| base);
        if let Some(value) = self.mixer.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.stress.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.shell(endpoint) {
            return Some(value);
        }
        if let Some(module) = endpoint
            .strip_prefix("ui.module.")
            .and_then(|value| value.strip_suffix(".collapsed"))
        {
            return Some(ReadValue::Bool(self.collapsed.contains(module)));
        }
        if let Some(index) = endpoint
            .strip_prefix("gallery.sprite.frame.")
            .and_then(|index| index.parse::<u8>().ok())
        {
            return Some(ReadValue::Scalar(f64::from(index)));
        }
        if let Some(name) = endpoint.strip_prefix("gallery.table.columns.width.") {
            consts::table_columns()
                .iter()
                .find(|column| column.id() == name)?;
            return self.table_widths.get(name).copied().map(ReadValue::Scalar);
        }
        if let Some(name) = endpoint.strip_prefix("gallery.table.columns.") {
            let index = consts::table_columns()
                .iter()
                .position(|column| column.id() == name)?;
            return Some(ReadValue::Bool(self.table_columns[index]));
        }
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
            "shader.energy" => ReadValue::Scalar(0.62),
            "shader.level" => ReadValue::Scalar(0.28),
            "gallery.motion.phase" => ReadValue::Scalar(f64::from(self.motion_phase)),
            "gallery.motion.clock" => ReadValue::Scalar(f64::from(self.motion_clock)),
            "gallery.sprite.scrub" => ReadValue::Scalar(f64::from(self.sprite_scrub)),
            "gallery.lottie.scrub" => ReadValue::Scalar(f64::from(self.lottie_scrub)),
            "vis.badge" | "deck.focused" => ReadValue::Bool(true),
            "vis.preset" => ReadValue::Scalar(self.vis_preset.as_()),
            "vis.time" => ReadValue::Scalar(self.vis_time_secs),
            "vis.preset_index" => ReadValue::Text(CATALOG.vis_indices[self.vis_preset]),
            "vis.preset_name" => ReadValue::Text(CATALOG.vis_presets[self.vis_preset]),
            "gallery.footer.deck" => ReadValue::Text("48kHz / 24bit"),
            "gallery.footer.deck_micro" => ReadValue::Text("READY"),
            "gallery.footer.global_bar" => ReadValue::Text("MASTER READY"),
            "gallery.footer.telemetry" => ReadValue::Text("LIVE"),
            "gallery.footer.layout" => ReadValue::Text("5 MODULES"),
            "gallery.footer.tokens_anatomy" => ReadValue::Text(CATALOG.footer_tokens_anatomy),
            "deck.playback.playing" => ReadValue::Bool(self.transport.playing()),
            "deck.playback.position_normalized" => {
                ReadValue::Scalar(self.transport.position_normalized())
            }
            "deck.playback.cached_normalized" => ReadValue::Scalar(consts::CACHED_NORMALIZED),
            "deck.playback.remaining_secs" => {
                ReadValue::Scalar(consts::DURATION_SECS - self.transport.position_secs())
            }
            "deck.playback.position_secs" => ReadValue::Scalar(self.transport.position_secs()),
            "deck.playback.duration_secs" => ReadValue::Scalar(consts::DURATION_SECS),
            "deck.playback.looping" => ReadValue::Bool(self.transport.loop_region().is_some()),
            "deck.playback.reverse" => ReadValue::Bool(self.transport.reverse()),
            "deck.playback.synced" | "demo.button.sync" => ReadValue::Bool(self.button_sync),
            "deck.playback.tempo" => ReadValue::Text(consts::TEMPO),
            "deck.playback.waveform" => ReadValue::Waveform(WaveformView {
                buckets: &self.waveform,
                revision: 0,
                beats: &self.wave_beats,
                downbeats: &self.wave_downbeats,
                unready: &consts::WAVE_UNREADY,
                bpm: Some(consts::BPM_VALUE),
                r#loop: self.transport.loop_region(),
                cues: self.transport.cues(),
            }),
            "deck.track.title" | "demo.track.title" => ReadValue::Text(CATALOG.title),
            "deck.track.source_kind" => ReadValue::Text(consts::ON_AIR),
            "demo.track.artist" => ReadValue::Text(CATALOG.artist),
            "engine.load" => ReadValue::Scalar(consts::ENGINE_LOAD),
            "engine.latency" => ReadValue::Text(consts::LATENCY),
            "ui.set.record_time" => ReadValue::Text(consts::RECORD_TIME),
            "deck.track.key" | "demo.key" => ReadValue::Text(consts::KEY),
            "deck.view.zoom" => ReadValue::Scalar(self.transport.zoom()),
            "player.output.levels" => ReadValue::Stereo(StereoLevels {
                l: if self.showing == "vis" {
                    self.vis_levels[0]
                } else {
                    0.66
                },
                r: if self.showing == "vis" {
                    self.vis_levels[1]
                } else {
                    0.52
                },
                volume: self.volume.as_(),
            }),
            "player.output.volume" | "demo.volume" => ReadValue::Scalar(self.volume),
            "library.visible_tracks" => ReadValue::Table(CATALOG.rows),
            "library.long_tracks" => ReadValue::Table(CATALOG.long_rows),
            "library.tree" => ReadValue::Tree(&self.tree_rows),
            "library.breadcrumb" => ReadValue::Text(CATALOG.breadcrumb),
            "library.query" => ReadValue::Text(&self.library_query),
            "library.scope" => ReadValue::Scalar(self.library_scope.as_()),
            "ui.preset" => ReadValue::Text("player"),
            "demo.bpm" => ReadValue::Text(consts::BPM),
            "demo.remain" | "deck.playback.remain" => ReadValue::Text(consts::REMAIN),
            "demo.knob.26" => ReadValue::Scalar(self.knobs[0]),
            "demo.knob.28" => ReadValue::Scalar(self.knobs[1]),
            "demo.knob.34" => ReadValue::Scalar(self.knobs[2]),
            "demo.knob.38" => ReadValue::Scalar(self.knobs[3]),
            "demo.levels" => ReadValue::Stereo(StereoLevels {
                l: 0.66,
                r: 0.52,
                volume: self.levels_volume.as_(),
            }),
            "demo.toggle.on" => ReadValue::Bool(self.toggle_on),
            "demo.toggle.off" => ReadValue::Bool(self.toggle_off),
            "demo.checkbox.on" => ReadValue::Bool(self.checkbox_on),
            "demo.checkbox.off" => ReadValue::Bool(self.checkbox_off),
            "demo.chip.active" => ReadValue::Bool(self.chip_active),
            "demo.chip.inactive" => ReadValue::Bool(self.chip_inactive),
            "demo.button.play" => ReadValue::Bool(self.button_play),
            "demo.button.cue" => ReadValue::Bool(self.button_cue),
            "demo.cells.segmented" => ReadValue::Scalar(self.segmented_index),
            "gallery.table.preset" => ReadValue::Scalar(self.table_preset.as_()),
            _ => return None,
        };
        Some(value)
    }
}

fn waveform() -> Vec<WaveBucket> {
    let total: f32 = consts::WAVE_BUCKETS.as_();
    (0..consts::WAVE_BUCKETS)
        .map(|index| {
            let high: f32 = ((index * 41 + 23) % 55).as_();
            let low: f32 = ((index * 17) % 70).as_();
            let mid: f32 = ((index * 29 + 11) % 65).as_();
            let phase: f32 = index.as_();
            let phase = phase / total;
            let envelope =
                (phase * 44.0).sin().mul_add(0.3, 0.62) * (phase * 5.0).cos().mul_add(0.18, 0.82);
            if consts::WAVE_UNREADY
                .iter()
                .any(|hole| phase >= hole[0] && phase < hole[1])
            {
                return WaveBucket::default();
            }
            WaveBucket {
                low: ((0.25 + low / 100.0) * envelope).clamp(0.0, 1.0),
                mid: ((0.18 + mid / 100.0) * envelope).clamp(0.0, 1.0),
                high: ((0.12 + high / 100.0) * envelope).clamp(0.0, 1.0),
            }
        })
        .collect()
}

fn beat_grid() -> (Vec<f32>, Vec<f32>) {
    let beat_count: usize = (consts::DURATION_SECS * f64::from(consts::BPM_VALUE) / 60.0)
        .floor()
        .as_();
    let beat_count_f: f32 = beat_count.as_();
    let beats: Vec<_> = (0..=beat_count)
        .map(|index| {
            let index: f32 = index.as_();
            index / beat_count_f
        })
        .collect();
    let downbeats = beats.iter().step_by(4).copied().collect();
    (beats, downbeats)
}
