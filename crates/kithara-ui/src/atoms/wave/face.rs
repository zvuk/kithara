use num_traits::cast::AsPrimitive;

use super::{
    bars::CoveragePalette,
    overlay::{Overlay, OverlayPalette},
    paint::{WavePaint, WavePalette},
    snapshot::{OverlayData, WaveformData},
};
use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    hosts::model::derived,
    module::WaveStyle,
    render::{ReadValue, Reads, Skin, WaveformView, Zoom, theme::RenderPalette},
    shaping::TextContext,
    skin::{ColorRole, WaveSkin},
};

/// The waveform a deck shows: the track's shape, where the playhead is in it,
/// and — on the hero wave — the panel naming what is loaded.
#[derive(Clone, PartialEq)]
pub(crate) struct Wave {
    render_palette: kithara_platform::sync::Arc<RenderPalette>,
    overlay_palette: OverlayPalette,
    background: Rgba,
    border: Rgba,
    cache_strip: Rgba,
    cue_badge: Rgba,
    cue_text: Rgba,
    palette: WavePalette,
    metrics: WaveSkin,
    pub(crate) style: WaveStyle,
}

/// What the wave is handed each frame.
#[derive(Clone, PartialEq)]
pub(crate) struct Drawn {
    pub(crate) overlay: Option<OverlayData>,
    pub(crate) waveform: Option<WaveformData>,
    pub(crate) zoom: Zoom,
    /// How far the host says the track is held, as a share of its length.
    pub(crate) cached: f32,
    pub(crate) progress: f32,
}

impl Wave {
    pub(crate) fn new(style: WaveStyle, skin: &Skin) -> Self {
        Self {
            style,
            render_palette: kithara_platform::sync::Arc::clone(&skin.palette),
            background: skin.rgba(skin.wave.background),
            border: skin.rgba(skin.wave.frame.border),
            cache_strip: Rgba {
                a: skin.wave.cache_strip_alpha,
                ..skin.rgba(skin.wave.cache_strip_color)
            },
            cue_badge: skin.rgba(skin.wave.cue_badge_background),
            cue_text: skin.rgba(skin.wave.cue_badge_text.color),
            metrics: skin.wave,
            overlay_palette: overlay_palette(skin),
            palette: WavePalette {
                coverage: CoveragePalette {
                    edge: skin.rgba(skin.wave.coverage_edge_color),
                    mark: skin.rgba(skin.wave.coverage_mark_color),
                },
                trough: skin.rgba(skin.wave.trough_color),
                grid: skin.rgba(skin.wave.grid_color),
                label: skin.rgba(skin.wave.label_color),
                played: skin.rgba(skin.wave.played_color),
                band_low: skin.rgba(skin.wave.band_low_color),
                band_mid: skin.rgba(skin.wave.band_mid_color),
                band_high: skin.rgba(skin.wave.band_high_color),
            },
        }
    }

    pub(crate) fn face<'a>(&'a self, data: &'a Drawn) -> WavePaint<'a> {
        WavePaint {
            background: self.background,
            border: self.border,
            cache_strip: self.cache_strip,
            cached: data.cached,
            cue_badge: self.cue_badge,
            cue_text: self.cue_text,
            metrics: self.metrics,
            overlay: data.overlay.as_ref().map(|overlay| Overlay {
                art: overlay.art.as_ref(),
                title: &overlay.title,
                artist: &overlay.artist,
                bpm: &overlay.bpm,
                key: &overlay.key,
                remain: &overlay.remain,
                badge: &overlay.badge,
                palette: OverlayPalette {
                    key: overlay
                        .key_color
                        .map_or(self.overlay_palette.key, |role| self.render_palette[role]),
                    ..self.overlay_palette
                },
            }),
            palette: self.palette,
            progress: data.progress,
            style: self.style,
            waveform: data.waveform.as_ref().map(|waveform| WaveformView {
                buckets: &waveform.buckets,
                revision: waveform.revision,
                beats: &waveform.beats,
                downbeats: &waveform.downbeats,
                unready: &waveform.unready,
                bpm: None,
                r#loop: waveform.loop_region,
                cues: &waveform.cues,
            }),
            zoom: data.zoom.into(),
        }
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
}

impl Drawn {
    /// What a reading shows when there is nothing to show.
    pub(crate) const EM_DASH: &str = "\u{2014}";

    /// Reads what a deck's wave shows: the shape from its own endpoint, the
    /// playhead and — on the hero wave — the words beside it from siblings in
    /// the same scope.
    pub(crate) fn read(
        style: WaveStyle,
        zoom: impl Into<Zoom>,
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
            cached: cached_extent(reads, scope, progress),
            overlay: (style == WaveStyle::Hero).then(|| {
                let bpm = waveform
                    .and_then(|view| view.bpm)
                    .map_or_else(|| Self::EM_DASH.to_owned(), |value| format!("{value:.2}"));
                OverlayData::read(reads, scope, bpm, badge.unwrap_or_default().to_owned())
            }),
            progress,
            waveform: waveform.map(WaveformData::from),
            zoom: zoom.into(),
        }
    }
}

impl OverlayData {
    pub(crate) fn read(reads: &dyn Reads, scope: &str, bpm: String, badge: String) -> Self {
        Self {
            art: read_art(reads, scope),
            title: read_text(reads, &derived("deck.track.title", scope))
                .filter(|title| !title.is_empty())
                .unwrap_or("No track loaded")
                .to_owned(),
            artist: read_text(reads, &derived("deck.track.source_kind", scope))
                .unwrap_or("no source")
                .to_owned(),
            bpm,
            key_color: track_key_color(reads, scope),
            key: read_text(reads, &derived("deck.track.key", scope))
                .filter(|key| !key.is_empty())
                .unwrap_or(Drawn::EM_DASH)
                .to_owned(),
            remain: read_text(reads, &derived("deck.playback.remain", scope))
                .unwrap_or(Drawn::EM_DASH)
                .to_owned(),
            badge,
        }
    }
}

fn overlay_palette(skin: &Skin) -> OverlayPalette {
    let metrics = skin.wave.overlay;
    let with_alpha = |color: Rgba, alpha: f32| Rgba { a: alpha, ..color };
    OverlayPalette {
        background: with_alpha(skin.rgba(metrics.background), metrics.background_alpha),
        art_background: skin.rgba(metrics.art_background),
        art_border: skin.rgba(metrics.art_frame.border),
        art_label: skin.rgba(metrics.art_label.color),
        title: skin.rgba(metrics.title.color),
        artist: skin.rgba(metrics.artist.color),
        readout_background: skin.rgba(metrics.readout_background),
        readout_border: skin.rgba(metrics.readout_frame.border),
        readout_label: skin.rgba(metrics.readout_label.color),
        bpm: skin.rgba(metrics.bpm_color),
        key: skin.rgba(metrics.key_color),
        remain: skin.rgba(metrics.remain_color),
        badge_background: skin.rgba(metrics.badge_background),
        badge_border: skin.rgba(metrics.badge_frame.border),
        badge_text: skin.rgba(metrics.badge_text.color),
    }
}

/// How far the track is held, as a share of its length.
///
/// The host owns the answer and a deck that answers nothing is not behind:
/// the playhead is the floor, so a wave with no cache endpoint draws the
/// played part alone.
pub(crate) fn cached_extent(reads: &dyn Reads, scope: &str, progress: f32) -> f32 {
    let played = progress.clamp(0.0, 1.0);
    match reads.get(&derived("deck.playback.cached_normalized", scope)) {
        Some(ReadValue::Scalar(cached)) => {
            let cached: f32 = cached.as_();
            cached.max(played).min(1.0)
        }
        _ => played,
    }
}

fn track_key_color(reads: &dyn Reads, scope: &str) -> Option<ColorRole> {
    match reads.get(&derived("deck.track.key_color", scope)) {
        Some(ReadValue::Color(role)) => Some(role),
        _ => None,
    }
}

fn read_text<'a>(reads: &'a dyn Reads, endpoint: &str) -> Option<&'a str> {
    match reads.get(endpoint) {
        Some(ReadValue::Text(value)) => Some(value),
        _ => None,
    }
}

fn read_art(reads: &dyn Reads, scope: &str) -> Option<crate::draw::Image> {
    match reads.get(&derived("deck.track.artwork", scope)) {
        Some(ReadValue::Image(image)) => Some(image.clone()),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::{Drawn, Rect, Skin, Wave, WaveStyle, cached_extent};
    use crate::{
        builtin,
        draw::{DrawCmd, DrawListBuilder, Geom, Image, ImageId, Paint, Pt, Rgba},
        ids::SourceUri,
        render::{ReadValue, Reads, WaveBucket, WaveformView},
        shaping::TextContext,
        skin::parse_skin_over,
    };

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
    fn what_is_held_never_falls_behind_the_playhead() {
        assert_eq!(cached_extent(&CacheReads(Some(0.1)), "@deck=a", 0.4), 0.4);
    }

    #[kithara::test]
    fn what_is_held_takes_the_answer_ahead_of_the_playhead() {
        assert_eq!(cached_extent(&CacheReads(Some(0.7)), "@deck=a", 0.4), 0.7);
    }

    #[kithara::test]
    fn what_is_held_stops_at_the_end_of_the_track() {
        assert_eq!(cached_extent(&CacheReads(Some(1.4)), "@deck=a", 0.4), 1.0);
    }

    #[kithara::test]
    fn a_deck_answering_nothing_holds_only_what_it_played() {
        assert_eq!(cached_extent(&CacheReads(None), "@deck=a", 0.4), 0.4);
    }

    #[kithara::test]
    fn the_answer_is_read_in_the_wave_own_scope() {
        assert_eq!(cached_extent(&CacheReads(Some(0.9)), "@deck=b", 0.4), 0.4);
    }

    pub(crate) struct WaveReads {
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
                    unready: &[],
                    bpm: Some(128.0),
                    r#loop: Some([0.2, 0.6]),
                    cues: &[0.4],
                })),
                "deck.playback.position_normalized" => Some(ReadValue::Scalar(0.3)),
                "deck.track.title" => Some(ReadValue::Text("Track")),
                "deck.track.source_kind" => Some(ReadValue::Text("Source")),
                "deck.track.key" => Some(ReadValue::Text("4m")),
                "deck.playback.remain" => Some(ReadValue::Text("-01:00")),
                _ => None,
            }
        }
    }

    pub(crate) fn reads() -> WaveReads {
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

    pub(crate) fn hero(skin: &Skin) -> (Wave, Drawn) {
        let reads = reads();
        let value = reads
            .get("deck.playback.waveform")
            .expect("the fixture must report a waveform");
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

    pub(crate) struct ArtReads(pub(crate) Option<Image>);

    impl Reads for ArtReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            if endpoint == "deck.track.artwork@deck=a" {
                self.0.as_ref().map(ReadValue::Image)
            } else {
                None
            }
        }
    }

    pub(crate) fn art(id: &str, width: u32, height: u32) -> Image {
        let len = usize::try_from(width * height * 4).expect("the fixture image fits usize");
        Image::pixels(
            ImageId::new(id),
            width,
            height,
            Arc::from(vec![255_u8; len]),
        )
        .expect("the fixture contains RGBA pixels")
    }

    #[kithara::test]
    fn artwork_is_drawn_instead_of_the_art_placeholder() {
        let skin = builtin::skin();
        let painter = Wave::new(WaveStyle::Hero, skin);
        let mut text = TextContext::from(skin.text_resources.as_ref());
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 640.0,
            h: 120.0,
        };
        for image in [None, Some(art("cover", 4, 2))] {
            let reads = ArtReads(image);
            let data = Drawn::read(WaveStyle::Hero, 1.0, Some("A"), None, &reads, "@deck=a");
            let mut list = DrawListBuilder::default();
            painter.paint(&mut list, &mut text, &data, bounds, true);
            let list = list.finish();
            let placeholder = list.commands().iter().any(
                |command| matches!(command, DrawCmd::Text { content, .. } if content == "ART"),
            );
            let drawn = list.commands().iter().find_map(|command| match command {
                DrawCmd::Clip { list, .. } => {
                    list.commands().iter().find_map(|command| match command {
                        DrawCmd::Image { image, .. } => Some(image.id()),
                        _ => None,
                    })
                }
                _ => None,
            });
            assert_eq!(placeholder, reads.0.is_none());
            assert_eq!(drawn, reads.0.as_ref().map(Image::id));
        }
    }

    #[kithara::test]
    fn a_long_deck_title_stays_on_one_line_inside_the_summary() {
        const TITLE: &str = "Big Man, Little Dignity (Re: DOM & JD BECK)";
        struct SummaryReads;
        impl Reads for SummaryReads {
            fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
                (endpoint == "deck.track.title@deck=a").then_some(ReadValue::Text(TITLE))
            }
        }

        let skin = builtin::skin();
        let painter = Wave::new(WaveStyle::Hero, skin);
        let data = Drawn::read(
            WaveStyle::Hero,
            1.0,
            Some("A"),
            None,
            &SummaryReads,
            "@deck=a",
        );
        let mut text = TextContext::from(skin.text_resources.as_ref());
        let mut list = DrawListBuilder::default();
        painter.paint(
            &mut list,
            &mut text,
            &data,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 440.0,
                h: 120.0,
            },
            true,
        );
        let list = list.finish();
        let (summary, run, at) = list
            .commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Clip { region, list } => {
                    list.commands().iter().find_map(|command| match command {
                        DrawCmd::Text {
                            run,
                            content,
                            transform,
                            ..
                        } if content.starts_with("Big Man") => {
                            Some((region, run, transform.apply(Pt { x: 0.0, y: 0.0 })))
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("the summary paints the title inside its clip");

        assert_eq!(
            run.height(),
            text.shape(TITLE, skin.wave.overlay.title, None).height()
        );
        assert!(at.x + run.width() <= summary.x + summary.w);
    }

    /// Every layer of the hero wave reaches the draw seam: the frame, the beat
    /// grid, the cue badge and the naming panel over them.
    #[kithara::test]
    fn a_hero_wave_paints_every_layer_through_the_draw_seam() {
        let skin = builtin::skin();
        let (painter, data) = hero(skin);
        let mut text = TextContext::from(skin.text_resources.as_ref());
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
        let grid = Rgba {
            a: skin.wave.grid_alpha,
            ..skin.rgba(skin.wave.grid_color)
        };
        assert!(list.commands().iter().any(|command| matches!(
            command,
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(found),
            } if *found == grid && rect.h > rect.w
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

    /// The playhead of a plain wave, drawn full height at the played edge.
    fn playhead_color(skin: &Skin) -> Rgba {
        let reads = reads();
        let value = reads
            .get("deck.playback.waveform")
            .expect("the fixture must report a waveform");
        let painter = Wave::new(WaveStyle::Default, skin);
        let data = Drawn::read(
            WaveStyle::Default,
            1.0,
            Some("A"),
            Some(&value),
            &reads,
            "@deck=a",
        );
        let bounds = Rect {
            h: 60.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        };
        let mut text = TextContext::from(skin.text_resources.as_ref());
        let mut list = DrawListBuilder::default();
        painter.paint(&mut list, &mut text, &data, bounds, false);
        let list = list.finish();
        list.commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    paint: Paint::Solid(color),
                } if rect.h == bounds.h && rect.w < skin.wave.playhead_marker_width => Some(*color),
                _ => None,
            })
            .expect("a plain wave must draw its playhead")
    }

    #[kithara::test]
    fn the_playhead_takes_the_colour_its_skin_role_names() {
        let skin = builtin::skin();

        assert_eq!(playhead_color(skin), skin.rgba(skin.wave.played_color));
    }

    #[kithara::test]
    fn the_playhead_follows_a_skin_written_over_the_builtin_one() {
        let origin = SourceUri("loud.kskin.ron".to_owned());
        let text = r##"(
            schema: "kithara.skin",
            version: 1,
            id: "kithara-loud",
            wave: (played_color: Danger),
        )"##;
        let document =
            parse_skin_over(builtin::skin_doc(), text, &origin).expect("the patch parses");
        let skin = Skin::resolve(document, builtin::text_doc(), &origin, &builtin::resolver())
            .expect("the patched document resolves");

        assert_eq!(playhead_color(&skin), skin.palette.danger);
    }

    #[kithara::test]
    fn the_hero_strip_colours_open_key_labels_and_draws_an_empty_or_absent_key_as_a_dash() {
        struct StripKeyReads(Option<&'static str>);

        impl Reads for StripKeyReads {
            fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
                match endpoint {
                    "deck.track.key@deck=a" => Some(ReadValue::Text(self.0?)),
                    "deck.track.key_color@deck=a" if self.0 == Some("11d") => {
                        Some(ReadValue::Color(crate::skin::ColorRole::Key11d))
                    }
                    _ => None,
                }
            }
        }

        let skin = builtin::skin();
        let painter = Wave::new(WaveStyle::Hero, skin);
        let bounds = Rect {
            h: 120.0,
            w: 640.0,
            x: 0.0,
            y: 0.0,
        };
        let drawn_key = |data: &Drawn| {
            let mut text = TextContext::from(skin.text_resources.as_ref());
            let mut list = DrawListBuilder::default();
            painter.paint(&mut list, &mut text, data, bounds, true);
            list.finish()
                .commands()
                .iter()
                .filter_map(|command| match command {
                    DrawCmd::Text { content, color, .. } => Some((content.to_string(), *color)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        let known = Drawn::read(
            WaveStyle::Hero,
            1.0,
            Some("A"),
            None,
            &StripKeyReads(Some("11d")),
            "@deck=a",
        );
        assert!(
            drawn_key(&known).contains(&("11d".to_owned(), skin.palette.key_11d)),
            "11d must paint in key_11d"
        );

        let fallback = skin.rgba(skin.wave.overlay.key_color);
        for missing in [None, Some("")] {
            let reads = StripKeyReads(missing);
            let immediate = Drawn::read(WaveStyle::Hero, 1.0, Some("A"), None, &reads, "@deck=a");
            assert_eq!(
                immediate
                    .overlay
                    .as_ref()
                    .map(|overlay| overlay.key.as_str()),
                Some(Drawn::EM_DASH),
            );
            assert!(
                drawn_key(&immediate).contains(&(Drawn::EM_DASH.to_owned(), fallback)),
                "missing {missing:?} must paint a dash in the overlay key_color"
            );
        }
    }
}
