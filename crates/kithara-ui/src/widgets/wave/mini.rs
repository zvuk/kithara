use std::{
    cell::Cell,
    hash::{DefaultHasher, Hash, Hasher},
};

use iced::{
    Color, Element, Event, Font, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Vertical,
    keyboard::{Event as KeyboardEvent, Modifiers},
    mouse::{Button, Cursor, Event as MouseEvent, Interaction},
    widget::{
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, Text as IcedCanvasText},
        text::{Alignment as TextAlignment, Shaping},
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    module::WaveStyle,
    render::{
        ReadValue, Reads, Skin, UiEvent, WaveBucket, fonts, model::derived, theme::RenderPalette,
    },
    skin::{FontSkin, FrameSkin, WaveOverlaySkin, WaveSkin},
    widgets::{
        Widget,
        behavior::{HoverState, ScalarDrag, ScalarDragMode, ScalarDragState, scroll_y},
        wave::{
            bars,
            hero::{HeroPalette, HeroWave, draw as draw_hero_wave},
            zoom_math::{clamp_zoom, window_bounds, x_to_norm, zoom_for_wheel},
        },
    },
};

#[derive(bon::Builder)]
pub(crate) struct MiniWave<'path, 'value, 'data, 'scope, 'reads, 'skin> {
    skin: &'skin Skin,
    reads: &'reads dyn Reads,
    path: &'path str,
    scope: &'scope str,
    badge: Option<&'path str>,
    value: Option<&'value ReadValue<'data>>,
    style: WaveStyle,
    zoom: f32,
}

impl<'a> Widget<'a> for MiniWave<'_, '_, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let waveform = match self.value {
            Some(ReadValue::Waveform(waveform)) => Some(*waveform),
            _ => None,
        };
        let bpm = waveform.and_then(|view| view.bpm);
        let progress = match self
            .reads
            .get(&derived("deck.playback.position_normalized", self.scope))
        {
            Some(ReadValue::Scalar(value)) => value.as_(),
            _ => 0.0,
        };
        let cached = cached_extent(self.reads, self.scope, progress);
        let zoom = clamp_zoom(self.zoom);
        let waveform = waveform.map(|view| WaveformData {
            buckets: view.buckets.to_vec().into_boxed_slice(),
            beats: view.beats.to_vec().into_boxed_slice(),
            downbeats: view.downbeats.to_vec().into_boxed_slice(),
            loop_region: view.r#loop,
            cues: view.cues.to_vec().into_boxed_slice(),
        });
        let show_beats = self.style == WaveStyle::Hero;
        let wave_revision = show_beats.then(|| wave_revision(waveform.as_ref(), progress, zoom));
        let overlay = show_beats.then(|| OverlayData {
            title: read_text(self.reads, &derived("deck.track.title", self.scope))
                .filter(|title| !title.is_empty())
                .unwrap_or("No track loaded")
                .to_owned(),
            artist: read_text(self.reads, &derived("deck.track.source_kind", self.scope))
                .unwrap_or("no source")
                .to_owned(),
            bpm: bpm.map_or_else(|| em_dash().to_owned(), |value| format!("{value:.2}")),
            key: read_text(self.reads, &derived("deck.track.key", self.scope))
                .unwrap_or(em_dash())
                .to_owned(),
            remain: read_text(self.reads, &derived("deck.playback.remain", self.scope))
                .unwrap_or(em_dash())
                .to_owned(),
            badge: self.badge.unwrap_or_default().to_owned(),
        });
        Canvas::new(MiniWaveCanvas {
            metrics: self.skin.wave,
            background: self.skin.color(self.skin.wave.background),
            border_color: self.skin.color(self.skin.wave.frame.border),
            cache_strip_color: with_alpha(
                self.skin.color(self.skin.wave.cache_strip_color),
                self.skin.wave.cache_strip_alpha,
            ),
            cue_badge_background: self.skin.color(self.skin.wave.cue_badge_background),
            cue_badge_text_color: self.skin.color(self.skin.wave.cue_badge_text_color),
            drag: ScalarDrag::builder()
                .path(self.path)
                .mode(if show_beats {
                    ScalarDragMode::RelativeHorizontal {
                        value: progress,
                        scale: zoom,
                    }
                } else {
                    ScalarDragMode::HorizontalClick
                })
                .hover(HoverState::new(if show_beats {
                    Interaction::Grab
                } else {
                    Interaction::Pointer
                }))
                .build(),
            overlay,
            overlay_palette: OverlayPalette::new(self.skin),
            palette: self.skin.palette,
            style: self.style,
            waveform,
            cached,
            progress,
            wave_revision,
            zoom,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct MiniWaveCanvas {
    background: Color,
    border_color: Color,
    cache_strip_color: Color,
    cue_badge_background: Color,
    cue_badge_text_color: Color,
    overlay: Option<OverlayData>,
    wave_revision: Option<u64>,
    waveform: Option<WaveformData>,
    overlay_palette: OverlayPalette,
    palette: RenderPalette,
    drag: ScalarDrag,
    metrics: WaveSkin,
    style: WaveStyle,
    cached: f32,
    progress: f32,
    zoom: f32,
}

#[derive(Default)]
struct MiniWaveState {
    wave: canvas::Cache,
    wave_revision: Cell<Option<u64>>,
    modifiers: Modifiers,
    loop_start: Option<f32>,
    drag: ScalarDragState,
}

struct WaveformData {
    beats: Box<[f32]>,
    buckets: Box<[WaveBucket]>,
    cues: Box<[f32]>,
    downbeats: Box<[f32]>,
    loop_region: Option<[f32; 2]>,
}

struct OverlayData {
    artist: String,
    badge: String,
    bpm: String,
    key: String,
    remain: String,
    title: String,
}

#[derive(Clone, Copy)]
struct ReadoutData<'a> {
    label: &'a str,
    value: &'a str,
    value_color: Color,
    framed: bool,
}

#[derive(Clone, Copy)]
struct CanvasText<'a> {
    content: &'a str,
    align_x: TextAlignment,
    color: Color,
    font: Font,
    skin: FontSkin,
    position: Point,
    align_y: Vertical,
    max_width: f32,
}

#[derive(Clone, Copy)]
struct OverlayPalette {
    art_background: Color,
    art_border: Color,
    art_label: Color,
    artist: Color,
    background: Color,
    badge_background: Color,
    badge_border: Color,
    badge_text: Color,
    bpm: Color,
    key: Color,
    readout_background: Color,
    readout_border: Color,
    readout_label: Color,
    remain: Color,
    title: Color,
}

impl OverlayPalette {
    const fn new(skin: &Skin) -> Self {
        let overlay = skin.wave.overlay;
        Self {
            background: with_alpha(skin.color(overlay.background), overlay.background_alpha),
            art_background: skin.color(overlay.art_background),
            art_border: skin.color(overlay.art_frame.border),
            art_label: skin.color(overlay.art_label_color),
            title: skin.color(overlay.title_color),
            artist: skin.color(overlay.artist_color),
            readout_background: skin.color(overlay.readout_background),
            readout_border: skin.color(overlay.readout_frame.border),
            readout_label: skin.color(overlay.readout_label_color),
            bpm: skin.color(overlay.bpm_color),
            key: skin.color(overlay.key_color),
            remain: skin.color(overlay.remain_color),
            badge_background: skin.color(overlay.badge_background),
            badge_border: skin.color(overlay.badge_frame.border),
            badge_text: skin.color(overlay.badge_text_color),
        }
    }
}

impl canvas::Program<UiEvent> for MiniWaveCanvas {
    type State = MiniWaveState;

    fn draw(
        &self,
        state: &MiniWaveState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let wave = self.wave_revision.map_or_else(
            || {
                let mut frame = Frame::new(renderer, bounds.size());
                self.draw_wave(&mut frame, bounds);
                frame.into_geometry()
            },
            |revision| {
                if state.wave_revision.get() != Some(revision) {
                    state.wave.clear();
                    state.wave_revision.set(Some(revision));
                }
                state.wave.draw(renderer, bounds.size(), |frame| {
                    self.draw_wave(frame, bounds);
                })
            },
        );

        let mut layers = vec![wave];
        if !self.hero() {
            let mut frame = Frame::new(renderer, bounds.size());
            let head_x = self.progress.clamp(0.0, 1.0) * bounds.width;
            frame.stroke(
                &Path::line(Point::new(head_x, 0.0), Point::new(head_x, bounds.height)),
                Stroke::default()
                    .with_color(self.palette.accent)
                    .with_width(self.metrics.playhead_width),
            );
            frame.fill_rectangle(
                Point::new(head_x - self.metrics.playhead_marker_width / 2.0, 0.0),
                Size::new(
                    self.metrics.playhead_marker_width,
                    self.metrics.playhead_marker_height,
                ),
                self.palette.accent,
            );
            if self.style == WaveStyle::Micro {
                self.draw_cache_strip(&mut frame, bounds, head_x);
            }
            layers.push(frame.into_geometry());
        }
        let header = overlay_strip(bounds, self.metrics.overlay);
        if let Some(overlay) = self.overlay.as_ref().filter(|_| !cursor.is_over(header)) {
            let mut frame = Frame::new(renderer, bounds.size());
            draw_overlay(
                &mut frame,
                bounds,
                overlay,
                self.metrics.overlay,
                self.overlay_palette,
            );
            layers.push(frame.into_geometry());
        }
        layers
    }

    fn mouse_interaction(
        &self,
        state: &MiniWaveState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Interaction {
        if self.has_waveform() {
            self.drag.mouse_interaction(&state.drag, bounds, cursor)
        } else {
            Interaction::default()
        }
    }

    fn update(
        &self,
        state: &mut MiniWaveState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        if let Event::Keyboard(KeyboardEvent::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
            return None;
        }
        if !self.has_waveform() {
            return None;
        }
        if self.hero() {
            match event {
                Event::Mouse(MouseEvent::ButtonPressed(Button::Left))
                    if state.modifiers.shift() && cursor.is_over(bounds) =>
                {
                    let start = self.track_position(bounds, cursor)?;
                    state.loop_start = Some(start);
                    return Some(self.drag.publish_child("loop_start", start));
                }
                Event::Mouse(MouseEvent::CursorMoved { .. }) if state.loop_start.is_some() => {
                    let end = self.track_position(bounds, cursor)?;
                    return Some(self.drag.publish_child("loop_end", end));
                }
                Event::Mouse(MouseEvent::ButtonReleased(Button::Left))
                    if state.loop_start.take().is_some() =>
                {
                    return Some(Action::capture());
                }
                _ => {}
            }
        }
        if self.hero()
            && let Event::Mouse(MouseEvent::WheelScrolled { delta }) = event
            && cursor.is_over(bounds)
        {
            let zoom = zoom_for_wheel(self.zoom, scroll_y(*delta));
            return Some(self.drag.publish_child("zoom", zoom));
        }
        self.drag.update(&mut state.drag, event, bounds, cursor)
    }
}

impl MiniWaveCanvas {
    fn draw_wave(&self, frame: &mut Frame, bounds: Rectangle) {
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.background);
        if let Some(waveform) = &self.waveform {
            if self.hero() {
                self.draw_zoom_wave(frame, bounds, waveform);
            } else {
                draw_bars(frame, bounds, &waveform.buckets, self.metrics, self.palette);
                if self.style == WaveStyle::Default {
                    bars::draw_played(
                        frame,
                        bounds,
                        self.progress.clamp(0.0, 1.0) * bounds.width,
                        self.metrics.overview_played_alpha,
                        self.palette,
                    );
                }
            }
        }
        draw_border(frame, bounds, self.metrics.frame, self.border_color);
    }

    fn draw_cache_strip(&self, frame: &mut Frame, bounds: Rectangle, head_x: f32) {
        let height = self.metrics.cache_strip_height;
        let top = (bounds.height - height).max(0.0);
        let cached_x = self.cached * bounds.width;
        let mut fill = |x: f32, width: f32, color| {
            frame.fill_rectangle(Point::new(x, top), Size::new(width, height), color);
        };
        fill(0.0, bounds.width, self.background);
        fill(0.0, head_x, self.palette.accent);
        fill(head_x, cached_x - head_x, self.cache_strip_color);
    }

    fn draw_zoom_wave(&self, frame: &mut Frame, bounds: Rectangle, data: &WaveformData) {
        draw_hero_wave(
            frame,
            bounds,
            HeroWave {
                beats: &data.beats,
                buckets: &data.buckets,
                cues: &data.cues,
                downbeats: &data.downbeats,
                loop_region: data.loop_region,
                position: self.progress,
                zoom: self.zoom,
            },
            self.metrics,
            HeroPalette {
                base: self.palette,
                cue_badge: self.cue_badge_background,
                cue_text: self.cue_badge_text_color,
            },
        );
    }

    fn has_waveform(&self) -> bool {
        self.waveform
            .as_ref()
            .is_some_and(|waveform| !waveform.buckets.is_empty())
    }

    const fn hero(&self) -> bool {
        matches!(self.style, WaveStyle::Hero)
    }

    fn track_position(&self, bounds: Rectangle, cursor: Cursor) -> Option<f32> {
        let position = cursor.position()?;
        let window = window_bounds(self.progress, self.zoom);
        x_to_norm(position.x - bounds.x, &window, bounds.width)
    }
}

fn wave_revision(waveform: Option<&WaveformData>, progress: f32, zoom: f32) -> u64 {
    let mut hasher = DefaultHasher::new();
    progress.to_bits().hash(&mut hasher);
    zoom.to_bits().hash(&mut hasher);
    if let Some(waveform) = waveform {
        waveform.buckets.len().hash(&mut hasher);
        for bucket in &waveform.buckets {
            bucket.low.to_bits().hash(&mut hasher);
            bucket.mid.to_bits().hash(&mut hasher);
            bucket.high.to_bits().hash(&mut hasher);
        }
        for mark in waveform.beats.iter().chain(waveform.downbeats.iter()) {
            mark.to_bits().hash(&mut hasher);
        }
        for cue in &waveform.cues {
            cue.to_bits().hash(&mut hasher);
        }
        if let Some([start, end]) = waveform.loop_region {
            start.to_bits().hash(&mut hasher);
            end.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn overlay_strip(bounds: Rectangle, metrics: WaveOverlaySkin) -> Rectangle {
    Rectangle::new(
        bounds.position(),
        Size::new(bounds.width, metrics.height.min(bounds.height)),
    )
}

fn draw_overlay(
    frame: &mut Frame,
    bounds: Rectangle,
    data: &OverlayData,
    metrics: WaveOverlaySkin,
    palette: OverlayPalette,
) {
    let height = overlay_strip(bounds, metrics).height;
    frame.fill_rectangle(
        Point::ORIGIN,
        Size::new(bounds.width, height),
        palette.background,
    );
    let summary_x = draw_art(frame, height, metrics, palette);
    let telemetry_x = draw_telemetry(frame, bounds.width, height, data, metrics, palette);
    draw_summary(
        frame,
        height,
        Rectangle::new(
            Point::new(summary_x, metrics.padding_y),
            Size::new(
                (telemetry_x - metrics.gap - summary_x).max(0.0),
                metrics.padding_y.mul_add(-2.0, height).max(0.0),
            ),
        ),
        data,
        metrics,
        palette,
    );
}

fn draw_art(
    frame: &mut Frame,
    height: f32,
    metrics: WaveOverlaySkin,
    palette: OverlayPalette,
) -> f32 {
    let content_y = (height - metrics.art_size) / 2.0;
    let art = Rectangle::new(
        Point::new(metrics.padding_x, content_y),
        Size::new(metrics.art_size, metrics.art_size),
    );
    draw_box(
        frame,
        art,
        metrics.art_frame,
        palette.art_background,
        palette.art_border,
    );
    draw_text(
        frame,
        CanvasText {
            content: "ART",
            position: art.center(),
            max_width: art.width,
            color: palette.art_label,
            skin: metrics.art_label,
            font: fonts::mono(metrics.art_label.weight),
            align_x: TextAlignment::Center,
            align_y: Vertical::Center,
        },
    );
    art.x + art.width + metrics.gap
}

fn draw_telemetry(
    frame: &mut Frame,
    width: f32,
    height: f32,
    data: &OverlayData,
    metrics: WaveOverlaySkin,
    palette: OverlayPalette,
) -> f32 {
    let readout_height = metrics
        .readout_height
        .min(metrics.padding_y.mul_add(-2.0, height).max(0.0));
    let readout_y = (height - readout_height) / 2.0;
    let mut right = width - metrics.padding_x;
    let badge = Rectangle::new(
        Point::new(
            right - metrics.badge_size,
            (height - metrics.badge_size) / 2.0,
        ),
        Size::new(metrics.badge_size, metrics.badge_size),
    );
    draw_box(
        frame,
        badge,
        metrics.badge_frame,
        palette.badge_background,
        palette.badge_border,
    );
    draw_text(
        frame,
        CanvasText {
            content: &data.badge,
            position: badge.center(),
            max_width: badge.width,
            color: palette.badge_text,
            skin: metrics.badge_text,
            font: fonts::display(metrics.badge_text.weight),
            align_x: TextAlignment::Center,
            align_y: Vertical::Center,
        },
    );
    right = badge.x - metrics.gap;

    let remain = Rectangle::new(
        Point::new(right - metrics.remain_width, readout_y),
        Size::new(metrics.remain_width, readout_height),
    );
    draw_readout(
        frame,
        remain,
        ReadoutData {
            label: "REMAIN",
            value: &data.remain,
            value_color: palette.remain,
            framed: false,
        },
        metrics,
        palette,
    );
    right = remain.x - metrics.gap;

    let key = Rectangle::new(
        Point::new(right - metrics.key_width, readout_y),
        Size::new(metrics.key_width, readout_height),
    );
    draw_readout(
        frame,
        key,
        ReadoutData {
            label: "KEY",
            value: &data.key,
            value_color: palette.key,
            framed: true,
        },
        metrics,
        palette,
    );
    right = key.x - metrics.gap;

    let bpm = Rectangle::new(
        Point::new(right - metrics.bpm_width, readout_y),
        Size::new(metrics.bpm_width, readout_height),
    );
    draw_readout(
        frame,
        bpm,
        ReadoutData {
            label: "BPM",
            value: &data.bpm,
            value_color: palette.bpm,
            framed: true,
        },
        metrics,
        palette,
    );
    bpm.x
}

fn draw_summary(
    frame: &mut Frame,
    height: f32,
    bounds: Rectangle,
    data: &OverlayData,
    metrics: WaveOverlaySkin,
    palette: OverlayPalette,
) {
    let total_text_height = metrics.title.size + metrics.summary_gap + metrics.artist.size;
    let title_y = (height - total_text_height) / 2.0;
    frame.with_clip(bounds, |frame| {
        draw_text(
            frame,
            CanvasText {
                content: &data.title,
                position: Point::new(bounds.x, title_y),
                max_width: bounds.width,
                color: palette.title,
                skin: metrics.title,
                font: fonts::display(metrics.title.weight),
                align_x: TextAlignment::Left,
                align_y: Vertical::Top,
            },
        );
        draw_text(
            frame,
            CanvasText {
                content: &data.artist,
                position: Point::new(bounds.x, title_y + metrics.title.size + metrics.summary_gap),
                max_width: bounds.width,
                color: palette.artist,
                skin: metrics.artist,
                font: fonts::sans(metrics.artist.weight),
                align_x: TextAlignment::Left,
                align_y: Vertical::Top,
            },
        );
    });
}

fn draw_readout(
    frame: &mut Frame,
    bounds: Rectangle,
    data: ReadoutData<'_>,
    metrics: WaveOverlaySkin,
    palette: OverlayPalette,
) {
    if data.framed {
        draw_box(
            frame,
            bounds,
            metrics.readout_frame,
            palette.readout_background,
            palette.readout_border,
        );
    }
    let inner_height = metrics
        .readout_padding_y
        .mul_add(-2.0, bounds.height)
        .max(0.0);
    let total_height =
        metrics.readout_label.size + metrics.readout_gap + metrics.readout_value.size;
    let label_y = bounds.y + metrics.readout_padding_y + (inner_height - total_height) / 2.0;
    let x = bounds.x + bounds.width - metrics.readout_padding_x;
    draw_text(
        frame,
        CanvasText {
            content: data.label,
            position: Point::new(x, label_y),
            max_width: metrics
                .readout_padding_x
                .mul_add(-2.0, bounds.width)
                .max(0.0),
            color: palette.readout_label,
            skin: metrics.readout_label,
            font: fonts::mono(metrics.readout_label.weight),
            align_x: TextAlignment::Right,
            align_y: Vertical::Top,
        },
    );
    draw_text(
        frame,
        CanvasText {
            content: data.value,
            position: Point::new(
                x,
                label_y + metrics.readout_label.size + metrics.readout_gap,
            ),
            max_width: metrics
                .readout_padding_x
                .mul_add(-2.0, bounds.width)
                .max(0.0),
            color: data.value_color,
            skin: metrics.readout_value,
            font: fonts::mono(metrics.readout_value.weight),
            align_x: TextAlignment::Right,
            align_y: Vertical::Top,
        },
    );
}

fn draw_box(
    frame: &mut Frame,
    bounds: Rectangle,
    skin: FrameSkin,
    background: Color,
    border: Color,
) {
    let path = Path::rounded_rectangle(bounds.position(), bounds.size(), skin.radius.into());
    frame.fill(&path, background);
    if skin.border_width > 0.0 {
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(border)
                .with_width(skin.border_width),
        );
    }
}

fn draw_text(frame: &mut Frame, text: CanvasText<'_>) {
    frame.fill_text(IcedCanvasText {
        content: text.content.to_owned(),
        position: text.position,
        max_width: text.max_width,
        color: text.color,
        size: text.skin.size.into(),
        font: text.font,
        align_x: text.align_x,
        align_y: text.align_y,
        shaping: Shaping::Advanced,
        ..IcedCanvasText::default()
    });
}

const fn em_dash() -> &'static str {
    "\u{2014}"
}

fn cached_extent(reads: &dyn Reads, scope: &str, progress: f32) -> f32 {
    let played = progress.clamp(0.0, 1.0);
    match reads.get(&derived("deck.playback.cached_normalized", scope)) {
        Some(ReadValue::Scalar(cached)) => {
            let cached: f32 = cached.as_();
            cached.max(played).min(1.0)
        }
        _ => played,
    }
}

fn read_text<'a>(reads: &'a dyn Reads, endpoint: &str) -> Option<&'a str> {
    match reads.get(endpoint) {
        Some(ReadValue::Text(value)) => Some(value),
        _ => None,
    }
}

fn draw_bars(
    frame: &mut Frame,
    bounds: Rectangle,
    buckets: &[WaveBucket],
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    let step = bars::step(metrics);
    let content_width = metrics.content_inset.mul_add(-2.0, bounds.width).max(0.0);
    let max_columns: usize = ((content_width + metrics.bar_gap) / step).floor().as_();
    let columns = max_columns.min(buckets.len());
    if columns == 0 {
        return;
    }

    let available_height = metrics.content_inset.mul_add(-2.0, bounds.height).max(0.0);
    for column in 0..columns {
        let start = column * buckets.len() / columns;
        let end = ((column + 1) * buckets.len() / columns)
            .max(start + 1)
            .min(buckets.len());
        let peak = buckets[start..end].iter().fold(
            WaveBucket {
                low: 0.0,
                mid: 0.0,
                high: 0.0,
            },
            |peak, bucket| WaveBucket {
                low: peak.low.max(bucket.low),
                mid: peak.mid.max(bucket.mid),
                high: peak.high.max(bucket.high),
            },
        );
        let column_x: f32 = column.as_();
        let center_x = column_x.mul_add(step, metrics.content_inset) + metrics.bar_width / 2.0;
        bars::draw_column(
            frame,
            bounds,
            center_x,
            peak,
            available_height,
            metrics,
            palette,
        );
    }
}

const fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

fn draw_border(frame: &mut Frame, bounds: Rectangle, skin: FrameSkin, color: Color) {
    if skin.border_width <= 0.0 {
        return;
    }
    let inset = skin.border_width / 2.0;
    let path = Path::rounded_rectangle(
        Point::new(inset, inset),
        Size::new(
            (bounds.width - skin.border_width).max(0.0),
            (bounds.height - skin.border_width).max(0.0),
        ),
        skin.radius.into(),
    );
    frame.stroke(
        &path,
        Stroke::default()
            .with_color(color)
            .with_width(skin.border_width),
    );
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn overlay_hides_only_when_the_cursor_is_inside_the_header_strip() {
        let metrics = crate::builtin::skin_doc().wave.overlay;
        let bounds = Rectangle::new(Point::new(100.0, 50.0), Size::new(400.0, 300.0));
        let strip = overlay_strip(bounds, metrics);

        let inside = Cursor::Available(Point::new(150.0, 50.0 + metrics.height / 2.0));
        let below = Cursor::Available(Point::new(150.0, 50.0 + metrics.height + 40.0));
        assert!(inside.is_over(strip));
        assert!(!below.is_over(strip));
    }

    struct CacheReads(Option<f64>);

    impl Reads for CacheReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            if endpoint == "deck.playback.cached_normalized@deck=a" {
                self.0.map(ReadValue::Scalar)
            } else {
                None
            }
        }
    }

    #[kithara::test]
    fn cached_extent_stays_at_the_playhead_when_the_host_answers_behind_it() {
        assert_eq!(cached_extent(&CacheReads(Some(0.1)), "@deck=a", 0.4), 0.4);
    }

    #[kithara::test]
    fn cached_extent_holds_an_answer_ahead_of_the_playhead() {
        assert_eq!(cached_extent(&CacheReads(Some(0.7)), "@deck=a", 0.4), 0.7);
        assert_eq!(cached_extent(&CacheReads(Some(1.4)), "@deck=a", 0.4), 1.0);
    }

    #[kithara::test]
    fn cached_extent_falls_back_to_the_playhead_without_a_host_answer() {
        assert_eq!(cached_extent(&CacheReads(None), "@deck=a", 0.4), 0.4);
        assert_eq!(cached_extent(&CacheReads(Some(0.9)), "@deck=b", 0.4), 0.4);
    }

    #[kithara::test]
    fn overlay_strip_clamps_to_short_bounds() {
        let metrics = crate::builtin::skin_doc().wave.overlay;
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(200.0, metrics.height / 2.0));
        assert_eq!(overlay_strip(bounds, metrics).height, bounds.height);
    }
}
