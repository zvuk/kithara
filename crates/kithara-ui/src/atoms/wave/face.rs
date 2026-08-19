use num_traits::cast::AsPrimitive;

use super::{
    overlay::{Overlay, OverlayPalette},
    paint::{WavePaint, WavePalette},
    snapshot::{OverlayData, WaveformData},
    zoom_math::clamp_zoom,
};
use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    module::WaveStyle,
    render::{ReadValue, Reads, Skin, WaveformView, model::derived},
    shaping::TextContext,
    skin::WaveSkin,
};

/// The waveform a deck shows: the track's shape, where the playhead is in it,
/// and — on the hero wave — the panel naming what is loaded.
pub(crate) struct Wave {
    background: Rgba,
    border: Rgba,
    cue_badge: Rgba,
    cue_text: Rgba,
    metrics: WaveSkin,
    overlay_palette: OverlayPalette,
    palette: WavePalette,
    style: WaveStyle,
}

/// What the wave is handed each frame.
pub(crate) struct Drawn {
    pub(crate) overlay: Option<OverlayData>,
    pub(crate) progress: f32,
    pub(crate) waveform: Option<WaveformData>,
    pub(crate) zoom: f32,
}

impl Wave {
    pub(crate) fn new(style: WaveStyle, skin: &Skin) -> Self {
        Self {
            background: skin.rgba(skin.wave.background),
            border: skin.rgba(skin.wave.frame.border),
            cue_badge: skin.rgba(skin.wave.cue_badge_background),
            cue_text: skin.rgba(skin.wave.cue_badge_text_color),
            metrics: skin.wave,
            overlay_palette: overlay_palette(skin),
            palette: WavePalette {
                bg_deep: skin.palette.bg_deep,
                line: skin.palette.line,
                text_dim: skin.palette.text_dim,
                accent: skin.palette.accent,
                accent_strong: skin.palette.accent_strong,
                wave_low: skin.palette.wave_low,
                wave_mid: skin.palette.wave_mid,
                wave_high: skin.palette.wave_high,
            },
            style,
        }
    }

    pub(crate) const fn hero(&self) -> bool {
        matches!(self.style, WaveStyle::Hero)
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Drawn,
        bounds: Rect,
        show_overlay: bool,
    ) {
        self.face(data).paint(list, text, bounds, show_overlay);
    }

    /// Where the naming panel sits, so a host can tell whether the pointer is
    /// on it.
    pub(crate) fn overlay_bounds(&self, data: &Drawn, bounds: Rect) -> Rect {
        self.face(data).overlay_bounds(bounds)
    }

    fn face<'a>(&'a self, data: &'a Drawn) -> WavePaint<'a> {
        WavePaint {
            background: self.background,
            border: self.border,
            cue_badge: self.cue_badge,
            cue_text: self.cue_text,
            metrics: self.metrics,
            overlay: data.overlay.as_ref().map(|overlay| Overlay {
                title: &overlay.title,
                artist: &overlay.artist,
                bpm: &overlay.bpm,
                key: &overlay.key,
                remain: &overlay.remain,
                badge: &overlay.badge,
                palette: self.overlay_palette,
            }),
            palette: self.palette,
            progress: data.progress,
            style: self.style,
            waveform: data.waveform.as_ref().map(|waveform| WaveformView {
                buckets: &waveform.buckets,
                revision: waveform.revision,
                beats: &waveform.beats,
                downbeats: &waveform.downbeats,
                bpm: None,
                r#loop: waveform.loop_region,
                cues: &waveform.cues,
            }),
            zoom: data.zoom,
        }
    }
}

impl Drawn {
    /// Reads what a deck's wave shows: the shape from its own endpoint, the
    /// playhead and — on the hero wave — the words beside it from siblings in
    /// the same scope.
    pub(crate) fn read(
        style: WaveStyle,
        zoom: f32,
        badge: Option<&str>,
        value: Option<&ReadValue<'_>>,
        reads: &dyn Reads,
        scope: &str,
    ) -> Self {
        let waveform = match value {
            Some(ReadValue::Waveform(waveform)) => Some(*waveform),
            _ => None,
        };
        let progress = match reads.get(&derived("deck.playback.position_normalized", scope)) {
            Some(ReadValue::Scalar(value)) => value.as_(),
            _ => 0.0,
        };
        Self {
            overlay: (style == WaveStyle::Hero).then(|| OverlayData {
                title: read_text(reads, &derived("deck.track.title", scope))
                    .filter(|title| !title.is_empty())
                    .unwrap_or("No track loaded")
                    .to_owned(),
                artist: read_text(reads, &derived("deck.track.source_kind", scope))
                    .unwrap_or("no source")
                    .to_owned(),
                bpm: waveform
                    .and_then(|view| view.bpm)
                    .map_or_else(|| EM_DASH.to_owned(), |value| format!("{value:.2}")),
                key: read_text(reads, &derived("deck.track.key", scope))
                    .unwrap_or(EM_DASH)
                    .to_owned(),
                remain: read_text(reads, &derived("deck.playback.remain", scope))
                    .unwrap_or(EM_DASH)
                    .to_owned(),
                badge: badge.unwrap_or_default().to_owned(),
            }),
            progress,
            waveform: waveform.map(WaveformData::from),
            zoom: clamp_zoom(zoom),
        }
    }

    #[cfg(any(feature = "masonry", test))]
    pub(crate) fn set_waveform(&mut self, view: WaveformView<'_>) -> bool {
        let waveform_changed = self
            .waveform
            .as_ref()
            .is_none_or(|waveform| !waveform.matches(view));
        if waveform_changed {
            self.waveform = Some(WaveformData::from(view));
        }
        let bpm = view
            .bpm
            .map_or_else(|| EM_DASH.to_owned(), |value| format!("{value:.2}"));
        let bpm_changed = self.overlay.as_mut().is_some_and(|overlay| {
            bpm != overlay.bpm && {
                overlay.bpm = bpm;
                true
            }
        });
        waveform_changed || bpm_changed
    }

    #[cfg(any(feature = "masonry", test))]
    pub(crate) fn refresh(&mut self, reads: &dyn Reads, scope: &str, zoom: Option<&str>) -> bool {
        let progress = match reads.get(&derived("deck.playback.position_normalized", scope)) {
            Some(ReadValue::Scalar(value)) => value.as_(),
            _ => 0.0,
        };
        let zoom = zoom
            .and_then(|endpoint| reads.get(endpoint))
            .and_then(|value| match value {
                ReadValue::Scalar(value) => Some(value.as_()),
                _ => None,
            })
            .map_or(self.zoom, clamp_zoom);
        let mut changed = std::mem::replace(&mut self.progress, progress) != progress;
        changed |= std::mem::replace(&mut self.zoom, zoom) != zoom;
        if let Some(overlay) = &mut self.overlay {
            let next = OverlayData {
                title: read_text(reads, &derived("deck.track.title", scope))
                    .filter(|title| !title.is_empty())
                    .unwrap_or("No track loaded")
                    .to_owned(),
                artist: read_text(reads, &derived("deck.track.source_kind", scope))
                    .unwrap_or("no source")
                    .to_owned(),
                bpm: overlay.bpm.clone(),
                key: read_text(reads, &derived("deck.track.key", scope))
                    .unwrap_or(EM_DASH)
                    .to_owned(),
                remain: read_text(reads, &derived("deck.playback.remain", scope))
                    .unwrap_or(EM_DASH)
                    .to_owned(),
                badge: overlay.badge.clone(),
            };
            changed |= std::mem::replace(overlay, next) != *overlay;
        }
        changed
    }

    pub(crate) fn has_waveform(&self) -> bool {
        self.waveform
            .as_ref()
            .is_some_and(|waveform| !waveform.buckets.is_empty())
    }
}

/// What a reading shows when there is nothing to show.
const EM_DASH: &str = "\u{2014}";

fn overlay_palette(skin: &Skin) -> OverlayPalette {
    let metrics = skin.wave.overlay;
    let with_alpha = |color: Rgba, alpha: f32| Rgba { a: alpha, ..color };
    OverlayPalette {
        background: with_alpha(skin.rgba(metrics.background), metrics.background_alpha),
        art_background: skin.rgba(metrics.art_background),
        art_border: skin.rgba(metrics.art_frame.border),
        art_label: skin.rgba(metrics.art_label_color),
        title: skin.rgba(metrics.title_color),
        artist: skin.rgba(metrics.artist_color),
        readout_background: skin.rgba(metrics.readout_background),
        readout_border: skin.rgba(metrics.readout_frame.border),
        readout_label: skin.rgba(metrics.readout_label_color),
        bpm: skin.rgba(metrics.bpm_color),
        key: skin.rgba(metrics.key_color),
        remain: skin.rgba(metrics.remain_color),
        badge_background: skin.rgba(metrics.badge_background),
        badge_border: skin.rgba(metrics.badge_frame.border),
        badge_text: skin.rgba(metrics.badge_text_color),
    }
}

fn read_text<'a>(reads: &'a dyn Reads, endpoint: &str) -> Option<&'a str> {
    match reads.get(endpoint) {
        Some(ReadValue::Text(value)) => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Drawn, Rect, Skin, Wave, WaveStyle};
    use crate::{
        builtin,
        draw::{DrawCmd, DrawListBuilder, Geom, Pt},
        render::{ReadValue, Reads, WaveBucket, WaveformView},
        shaping::TextContext,
    };

    struct WaveReads {
        buckets: [WaveBucket; 2],
    }

    impl Reads for WaveReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            let endpoint = endpoint.split_once('@').map_or(endpoint, |(id, _)| id);
            match endpoint {
                "deck.playback.waveform" => Some(ReadValue::Waveform(WaveformView {
                    buckets: &self.buckets,
                    revision: 0,
                    beats: &[0.25],
                    downbeats: &[0.5],
                    bpm: Some(128.0),
                    r#loop: Some([0.2, 0.6]),
                    cues: &[0.4],
                })),
                "deck.playback.position_normalized" => Some(ReadValue::Scalar(0.3)),
                "deck.track.title" => Some(ReadValue::Text("Track")),
                "deck.track.source_kind" => Some(ReadValue::Text("Source")),
                "deck.track.key" => Some(ReadValue::Text("8A")),
                "deck.playback.remain" => Some(ReadValue::Text("-01:00")),
                _ => None,
            }
        }
    }

    fn reads() -> WaveReads {
        WaveReads {
            buckets: [
                WaveBucket {
                    low: 0.25,
                    mid: 0.5,
                    high: 0.75,
                },
                WaveBucket {
                    low: 0.75,
                    mid: 0.5,
                    high: 0.25,
                },
            ],
        }
    }

    fn hero(skin: &Skin) -> (Wave, Drawn) {
        let reads = reads();
        let value = reads
            .get("deck.playback.waveform")
            .unwrap_or_else(|| panic!("the fixture must report a waveform"));
        (
            Wave::new(WaveStyle::Hero, skin),
            Drawn::read(
                WaveStyle::Hero,
                1.0,
                Some("A"),
                Some(&value),
                &reads,
                "@deck=a",
            ),
        )
    }

    struct UpdatedReads;

    impl Reads for UpdatedReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            match endpoint {
                "deck.playback.position_normalized@deck=a" => Some(ReadValue::Scalar(0.75)),
                "deck.playback.remain@deck=a" => Some(ReadValue::Text("-00:15")),
                "deck.track.key@deck=a" => Some(ReadValue::Text("9A")),
                "deck.track.source_kind@deck=a" => Some(ReadValue::Text("file")),
                "deck.track.title@deck=a" => Some(ReadValue::Text("Updated")),
                "deck.waveform.zoom@deck=a" => Some(ReadValue::Scalar(2.0)),
                _ => None,
            }
        }
    }

    /// A retained wave owns all scoped words around its primary waveform, so
    /// analysis and playback updates do not need a document rebuild.
    #[kithara::test]
    fn a_retained_wave_refreshes_its_scoped_snapshot() {
        let skin = builtin::skin();
        let (_, mut data) = hero(skin);

        assert!(data.refresh(&UpdatedReads, "@deck=a", Some("deck.waveform.zoom@deck=a")));

        let overlay = data
            .overlay
            .as_ref()
            .unwrap_or_else(|| panic!("a hero wave must keep its naming panel"));
        assert_eq!(overlay.title, "Updated");
        assert_eq!(overlay.artist, "file");
        assert_eq!(overlay.key, "9A");
        assert_eq!(overlay.remain, "-00:15");
        assert_eq!(data.progress, 0.75);
        assert_eq!(data.zoom, 0.5);
    }

    /// The continuously repainted wave keeps its owned sample arrays when the
    /// borrowed view has not changed, while still taking a new BPM reading.
    #[kithara::test]
    fn an_unchanged_waveform_is_not_copied_each_frame() {
        let skin = builtin::skin();
        let reads = reads();
        let (_, mut data) = hero(skin);
        let buckets = data
            .waveform
            .as_ref()
            .unwrap_or_else(|| panic!("the fixture must own waveform samples"))
            .buckets
            .as_ptr();
        let value = reads
            .get("deck.playback.waveform")
            .unwrap_or_else(|| panic!("the fixture must report a waveform"));
        let ReadValue::Waveform(mut view) = value else {
            panic!("the waveform endpoint must report a waveform");
        };

        assert!(!data.set_waveform(view));
        assert_eq!(
            data.waveform
                .as_ref()
                .map(|waveform| waveform.buckets.as_ptr()),
            Some(buckets)
        );
        view.bpm = Some(129.0);
        assert!(data.set_waveform(view));
        assert_eq!(
            data.waveform
                .as_ref()
                .map(|waveform| waveform.buckets.as_ptr()),
            Some(buckets)
        );
        assert_eq!(
            data.overlay.as_ref().map(|overlay| overlay.bpm.as_str()),
            Some("129.00")
        );
    }

    /// Every layer of the hero wave reaches the draw seam: the frame, the beat
    /// grid, the cue badge and the naming panel over them.
    #[kithara::test]
    fn a_hero_wave_paints_every_layer_through_the_draw_seam() {
        let skin = builtin::skin();
        let (painter, data) = hero(skin);
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        painter.paint(
            &mut list,
            &mut text,
            &data,
            Rect {
                h: 120.0,
                w: 640.0,
                x: 0.0,
                y: 0.0,
            },
            true,
        );
        let list = list.finish();

        assert!(list.commands().iter().any(|command| matches!(
            command,
            DrawCmd::Fill {
                geom: Geom::Rect(_),
                ..
            }
        )));
        assert!(list.commands().iter().any(|command| matches!(
            command,
            DrawCmd::Stroke {
                geom: Geom::Line { .. },
                ..
            }
        )));
        assert!(list.commands().iter().any(|command| matches!(
            command,
            DrawCmd::Text { content, .. } if content == "1"
        )));
        assert!(list.commands().iter().any(|command| matches!(
            command,
            DrawCmd::Clip { list, .. } if list.commands().iter().any(|nested| matches!(
                nested,
                DrawCmd::Text { content, .. } if content == "Track"
            ))
        )));
    }

    /// The panel steps aside for a pointer on it, not for one anywhere on the
    /// wave, so the box a host tests against has to be the panel's own.
    #[kithara::test]
    fn the_panel_covers_the_top_of_the_wave_and_no_more() {
        let skin = builtin::skin();
        let (painter, data) = hero(skin);
        let bounds = Rect {
            h: 300.0,
            w: 400.0,
            x: 100.0,
            y: 50.0,
        };
        let panel = painter.overlay_bounds(&data, bounds);

        assert!(panel.contains(Pt {
            x: 150.0,
            y: 50.0 + skin.wave.overlay.height / 2.0,
        }));
        assert!(!panel.contains(Pt {
            x: 150.0,
            y: 50.0 + skin.wave.overlay.height + 40.0,
        }));
    }

    /// And on a wave shorter than the panel it stops at the wave, rather than
    /// hanging below it.
    #[kithara::test]
    fn the_panel_clamps_to_a_short_wave() {
        let skin = builtin::skin();
        let (painter, data) = hero(skin);
        let bounds = Rect {
            h: skin.wave.overlay.height / 2.0,
            w: 200.0,
            x: 0.0,
            y: 0.0,
        };

        assert_eq!(painter.overlay_bounds(&data, bounds).h, bounds.h);
    }
}
