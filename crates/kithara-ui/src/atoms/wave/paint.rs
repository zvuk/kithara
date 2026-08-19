use num_traits::cast::AsPrimitive;

use super::{
    bars,
    hero::{self, HeroPalette, HeroWave},
    overlay::{self, Overlay},
};
use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba},
    module::WaveStyle,
    render::{WaveBucket, WaveformView},
    shaping::TextContext,
    skin::{FrameSkin, WaveSkin},
};

#[derive(Clone, Copy)]
pub(crate) struct WavePalette {
    pub(crate) bg_deep: Rgba,
    pub(crate) line: Rgba,
    pub(crate) text_dim: Rgba,
    pub(crate) accent: Rgba,
    pub(crate) accent_strong: Rgba,
    pub(crate) wave_low: Rgba,
    pub(crate) wave_mid: Rgba,
    pub(crate) wave_high: Rgba,
}

#[derive(Clone, Copy)]
pub(crate) struct WavePaint<'a> {
    pub(crate) background: Rgba,
    pub(crate) border: Rgba,
    pub(crate) cue_badge: Rgba,
    pub(crate) cue_text: Rgba,
    pub(crate) metrics: WaveSkin,
    pub(crate) overlay: Option<Overlay<'a>>,
    pub(crate) palette: WavePalette,
    pub(crate) progress: f32,
    pub(crate) style: WaveStyle,
    pub(crate) waveform: Option<WaveformView<'a>>,
    pub(crate) zoom: f32,
}

impl WavePaint<'_> {
    pub(crate) const fn hero(&self) -> bool {
        matches!(self.style, WaveStyle::Hero)
    }

    pub(crate) fn overlay_bounds(&self, bounds: Rect) -> Rect {
        overlay::strip(bounds, self.metrics.overlay)
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        show_overlay: bool,
    ) {
        self.paint_wave(list, text, bounds);
        self.paint_foreground(list, text, bounds, show_overlay);
    }

    pub(crate) fn paint_wave(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
    ) {
        list.fill_rect(bounds, self.background);
        if let Some(waveform) = self.waveform {
            if self.hero() {
                hero::draw(
                    list,
                    text,
                    bounds,
                    HeroWave {
                        beats: waveform.beats,
                        buckets: waveform.buckets,
                        cues: waveform.cues,
                        downbeats: waveform.downbeats,
                        loop_region: waveform.r#loop,
                        position: self.progress,
                        zoom: self.zoom,
                    },
                    self.metrics,
                    HeroPalette {
                        base: self.palette,
                        cue_badge: self.cue_badge,
                        cue_text: self.cue_text,
                    },
                );
            } else {
                let played = (self.style == WaveStyle::Default).then(|| {
                    bars::Played::new(
                        bounds.x + self.progress.clamp(0.0, 1.0) * bounds.w,
                        self.metrics.overview_played_alpha,
                        self.palette.bg_deep,
                    )
                });
                draw_bars(
                    list,
                    bounds,
                    waveform.buckets,
                    self.metrics,
                    self.palette,
                    played,
                );
            }
        }
        draw_border(list, bounds, self.metrics.frame, self.border);
    }

    pub(crate) fn paint_foreground(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        show_overlay: bool,
    ) {
        if !self.hero() {
            let head_x = bounds.x + self.progress.clamp(0.0, 1.0) * bounds.w;
            list.stroke_line(
                Pt {
                    x: head_x,
                    y: bounds.y,
                },
                Pt {
                    x: head_x,
                    y: bounds.y + bounds.h,
                },
                self.palette.accent_strong,
                self.metrics.playhead_width,
            );
        }
        if show_overlay && let Some(data) = self.overlay {
            overlay::paint(list, text, bounds, data, self.metrics.overlay);
        }
    }
}

fn draw_bars(
    list: &mut DrawListBuilder,
    bounds: Rect,
    buckets: &[WaveBucket],
    metrics: WaveSkin,
    palette: WavePalette,
    played: Option<bars::Played>,
) {
    let step = bars::step(metrics);
    let columns = bars::columns(bounds, metrics).min(buckets.len());
    if columns == 0 {
        return;
    }

    let available_height = (bounds.h - metrics.content_inset * 2.0).max(0.0);
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
        let center_x = bounds.x + metrics.content_inset + column_x * step + metrics.bar_width / 2.0;
        let colors = [palette.wave_low, palette.wave_mid, palette.wave_high];
        bars::draw_column(
            list,
            bounds,
            center_x,
            peak,
            available_height,
            metrics,
            played.map_or(colors, |played| played.colors(center_x, colors)),
        );
    }
}

fn draw_border(list: &mut DrawListBuilder, bounds: Rect, skin: FrameSkin, color: Rgba) {
    if skin.border_width <= 0.0 {
        return;
    }
    let inset = skin.border_width / 2.0;
    list.stroke_rounded_rect(
        Rect {
            h: (bounds.h - skin.border_width).max(0.0),
            w: (bounds.w - skin.border_width).max(0.0),
            x: bounds.x + inset,
            y: bounds.y + inset,
        },
        skin.radius,
        color,
        skin.border_width,
    );
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::wave::overlay::OverlayPalette,
        builtin,
        draw::{DrawCmd, Geom, Paint, Transform},
    };

    #[kithara::test]
    fn hero_wave_records_waveform_semantics_and_overlay() {
        let buckets = [WaveBucket {
            low: 0.8,
            mid: 0.6,
            high: 0.4,
        }];
        let beats = [0.25];
        let downbeats = [0.5];
        let cues = [0.75];
        let palette = WavePalette {
            bg_deep: color(0.1),
            line: color(0.2),
            text_dim: color(0.3),
            accent: color(0.4),
            accent_strong: color(0.5),
            wave_low: color(0.6),
            wave_mid: color(0.7),
            wave_high: color(0.8),
        };
        let cue_badge = color(0.9);
        let overlay_palette = overlay_palette();
        let mut metrics = builtin::skin().wave;
        metrics.frame.border_width = 1.0;
        let paint = WavePaint {
            background: color(0.01),
            border: color(0.02),
            cue_badge,
            cue_text: color(0.03),
            metrics,
            overlay: Some(Overlay {
                title: "Track",
                artist: "Artist",
                bpm: "120.00",
                key: "Am",
                remain: "-03:20",
                badge: "A",
                palette: overlay_palette,
            }),
            palette,
            progress: 0.5,
            style: WaveStyle::Hero,
            waveform: Some(WaveformView {
                buckets: &buckets,
                revision: 0,
                beats: &beats,
                downbeats: &downbeats,
                bpm: Some(120.0),
                r#loop: Some([0.3, 0.7]),
                cues: &cues,
            }),
            zoom: 1.0,
        };
        let bounds = Rect {
            h: 200.0,
            w: 800.0,
            x: 11.0,
            y: 17.0,
        };
        let mut text = TextContext::from(builtin::skin().text_resources());
        let mut list = DrawListBuilder::default();

        paint.paint_wave(&mut list, &mut text, bounds);
        paint.paint_foreground(&mut list, &mut text, bounds, true);

        let list = list.finish();
        let commands = list.commands();
        assert_eq!(fill_rect(commands, paint.background), Some(bounds));
        assert_eq!(
            fill_rect(commands, cue_badge).map(|rect| rect.y),
            Some(bounds.y)
        );
        assert!(has_fill(commands, palette.accent));
        assert!(has_fill(commands, overlay_palette.background));
        assert!(has_stroke(commands, paint.border));
        for expected in [
            palette.wave_low,
            palette.wave_mid,
            palette.wave_high,
            cue_badge,
        ] {
            assert!(has_fill(commands, expected));
        }
        assert!(has_fill(
            commands,
            Rgba {
                a: paint.metrics.loop_fill_alpha,
                ..palette.accent
            }
        ));
        assert!(!has_fill(
            commands,
            Rgba {
                a: paint.metrics.played_alpha,
                ..palette.bg_deep
            }
        ));
        assert!(has_line(
            commands,
            Rgba {
                a: paint.metrics.grid_alpha,
                ..palette.line
            }
        ));
        assert!(has_line(
            commands,
            Rgba {
                a: paint.metrics.downbeat_alpha,
                ..palette.text_dim
            }
        ));
        assert!(has_line(commands, palette.accent));
        assert!(has_line(commands, palette.wave_high));
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, DrawCmd::Text { content, .. } if content == "1"))
        );
        assert!(commands.iter().any(|command| {
            matches!(command, DrawCmd::Clip { list, .. } if list.commands().iter().any(
                |nested| matches!(nested, DrawCmd::Text { content, .. } if content == "Track")
            ))
        }));
        let overlay = paint.metrics.overlay;
        let header = overlay::strip(bounds, overlay);
        assert_eq!(
            fill_rect(commands, overlay_palette.background),
            Some(header)
        );
        assert_eq!(
            text_transform(commands, "Track"),
            Some(Transform::translate(Pt {
                x: bounds.x + overlay.padding_x + overlay.art_size + overlay.gap,
                y: bounds.y
                    + (header.h - (overlay.title.size + overlay.summary_gap + overlay.artist.size))
                        / 2.0,
            }))
        );
    }

    #[kithara::test]
    fn overview_styles_paint_bars_and_playhead_through_the_draw_seam() {
        let buckets = [WaveBucket {
            low: 0.8,
            mid: 0.6,
            high: 0.4,
        }];
        let palette = WavePalette {
            bg_deep: color(0.1),
            line: color(0.2),
            text_dim: color(0.3),
            accent: color(0.4),
            accent_strong: color(0.5),
            wave_low: color(0.6),
            wave_mid: color(0.7),
            wave_high: color(0.8),
        };
        let metrics = builtin::skin().wave;
        let bounds = Rect {
            h: 60.0,
            w: 320.0,
            x: 13.0,
            y: 19.0,
        };

        for style in [WaveStyle::Default, WaveStyle::Micro] {
            let paint = WavePaint {
                background: color(0.01),
                border: color(0.02),
                cue_badge: color(0.03),
                cue_text: color(0.04),
                metrics,
                overlay: None,
                palette,
                progress: 0.5,
                style,
                waveform: Some(WaveformView {
                    buckets: &buckets,
                    revision: 0,
                    beats: &[],
                    downbeats: &[],
                    bpm: None,
                    r#loop: None,
                    cues: &[],
                }),
                zoom: 1.0,
            };
            let mut text = TextContext::from(builtin::skin().text_resources());
            let mut list = DrawListBuilder::default();

            paint.paint(&mut list, &mut text, bounds, false);

            let list = list.finish();
            let commands = list.commands();
            let colors = [palette.wave_low, palette.wave_mid, palette.wave_high];
            let expected = if style == WaveStyle::Default {
                let center_x = bounds.x + metrics.content_inset + metrics.bar_width / 2.0;
                bars::Played::new(
                    bounds.x + paint.progress * bounds.w,
                    metrics.overview_played_alpha,
                    palette.bg_deep,
                )
                .colors(center_x, colors)
            } else {
                colors
            };
            for color in expected {
                assert!(has_fill(commands, color));
            }
            assert!(has_line(commands, palette.accent_strong));
            assert!(!has_fill(
                commands,
                Rgba {
                    a: metrics.overview_played_alpha,
                    ..palette.bg_deep
                }
            ));
        }
    }

    fn has_fill(commands: &[DrawCmd], color: Rgba) -> bool {
        commands
            .iter()
            .any(|command| matches!(command, DrawCmd::Fill { paint: Paint::Solid(found), .. } if *found == color))
    }

    fn has_line(commands: &[DrawCmd], color: Rgba) -> bool {
        commands.iter().any(|command| {
            matches!(
                command,
                DrawCmd::Stroke {
                    geom: Geom::Line { .. },
                    color: found,
                    ..
                } if *found == color
            )
        })
    }

    fn has_stroke(commands: &[DrawCmd], color: Rgba) -> bool {
        commands.iter().any(
            |command| matches!(command, DrawCmd::Stroke { color: found, .. } if *found == color),
        )
    }

    fn fill_rect(commands: &[DrawCmd], color: Rgba) -> Option<Rect> {
        commands.iter().find_map(|command| match command {
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(found),
            } if *found == color => Some(*rect),
            _ => None,
        })
    }

    fn text_transform(commands: &[DrawCmd], expected: &str) -> Option<Transform> {
        commands.iter().find_map(|command| match command {
            DrawCmd::Text {
                content, transform, ..
            } if content == expected => Some(*transform),
            DrawCmd::Clip { list, .. } => text_transform(list.commands(), expected),
            _ => None,
        })
    }

    fn overlay_palette() -> OverlayPalette {
        OverlayPalette {
            background: color(0.11),
            art_background: color(0.12),
            art_border: color(0.13),
            art_label: color(0.14),
            title: color(0.15),
            artist: color(0.16),
            readout_background: color(0.17),
            readout_border: color(0.18),
            readout_label: color(0.19),
            bpm: color(0.21),
            key: color(0.22),
            remain: color(0.23),
            badge_background: color(0.24),
            badge_border: color(0.25),
            badge_text: color(0.26),
        }
    }

    const fn color(r: f32) -> Rgba {
        Rgba {
            a: 1.0,
            b: 0.0,
            g: 0.0,
            r,
        }
    }
}
