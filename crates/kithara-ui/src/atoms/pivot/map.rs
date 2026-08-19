use crate::{
    draw::{DrawListBuilder, FillRule, Pt, Rect, Rgba, Transform, Verb},
    render::{PortalMapView, PortalTarget, Skin},
    shaping::TextContext,
    skin::{ColorRole, FontFamily, PortalMapSkin, TextRoleSkin},
};

/// A tempo axis with one arc from the master tempo to each target.
pub(crate) struct PortalMap {
    accent: Rgba,
    background: Rgba,
    line: Rgba,
    line_inner: Rgba,
    metrics: PortalMapSkin,
    muted: Rgba,
    role: TextRoleSkin,
    selected: Rgba,
}

/// The tempi a portal map draws, owned for as long as the control is mounted.
///
/// The renderer-facing view borrows its targets from the frame that built it,
/// while a retained host keeps its leaf across frames, so the map owns the
/// snapshot rather than the slice behind it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PortalMapData {
    pub(crate) master: f32,
    pub(crate) min: f32,
    pub(crate) max: f32,
    pub(crate) targets: Vec<PortalTarget>,
}

impl From<PortalMapView<'_>> for PortalMapData {
    fn from(view: PortalMapView<'_>) -> Self {
        Self {
            master: view.master,
            min: view.min,
            max: view.max,
            targets: view.targets.to_vec(),
        }
    }
}

impl PortalMap {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.portal_map;
        Self {
            accent: skin.palette.accent,
            background: skin.palette.bg_inset,
            line: skin.palette.line,
            line_inner: skin.palette.line_inner,
            metrics,
            muted: skin.palette.muted,
            role: TextRoleSkin {
                color: ColorRole::Muted,
                font: FontFamily::Mono,
                size: metrics.label.size,
                spacing: 0.0,
                weight: metrics.label.weight,
            },
            selected: skin.palette.wave_high,
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &PortalMapData,
        bounds: Rect,
    ) {
        list.fill_rect(bounds, self.background);
        let axis = self.axis(bounds);
        list.fill_rect(axis, self.line);
        let Some(scale) = Scale::new(data.min, data.max, axis) else {
            return;
        };
        self.ticks(list, text, scale, axis.y);
        let master_x = scale.x(data.master);
        for target in &data.targets {
            self.arc(
                list,
                master_x,
                scale.x(target.bpm),
                axis.y,
                target.is_selected,
            );
        }
        self.marker(list, master_x, axis.y, self.accent);
        if let Some(target) = data.targets.iter().find(|target| target.is_selected) {
            self.marker(list, scale.x(target.bpm), axis.y, self.selected);
        }
    }

    /// The hairline the tempi are measured against, inset from both ends so the
    /// outermost label still has room beside it.
    fn axis(&self, bounds: Rect) -> Rect {
        Rect {
            h: 1.0,
            w: (bounds.w - self.metrics.axis_inset_x * 2.0).max(0.0),
            x: bounds.x + self.metrics.axis_inset_x.min(bounds.w / 2.0),
            y: bounds.y + (bounds.h - self.metrics.axis_offset_bottom).max(0.0),
        }
    }

    fn ticks(&self, list: &mut DrawListBuilder, text: &mut TextContext, scale: Scale, axis_y: f32) {
        let step = self.metrics.tick_step;
        if !step.is_finite() || step <= 0.0 || scale.span / step > MAX_TICKS {
            return;
        }
        let mut bpm = (scale.min / step).ceil() * step;
        let max = scale.min + scale.span;
        while bpm <= max {
            let x = scale.x(bpm).round();
            list.fill_rect(
                Rect {
                    h: self.metrics.tick_height,
                    w: 1.0,
                    x,
                    y: axis_y - self.metrics.tick_height,
                },
                self.line,
            );
            let label = format!("{bpm:.0}");
            let run = text.shape(&label, self.role, None);
            list.text(
                &run,
                &label,
                Transform::translate(Pt {
                    x: x - self.metrics.label_offset_x,
                    y: axis_y + self.metrics.label_offset_y - run.height() / 2.0,
                }),
                self.muted,
            );
            bpm += step;
        }
    }

    /// One arc between two tempi, rising by a share of the distance between
    /// them so a near portal reads as a short hop and a far one as a long one.
    fn arc(
        &self,
        list: &mut DrawListBuilder,
        master_x: f32,
        target_x: f32,
        axis_y: f32,
        selected: bool,
    ) {
        let radius = (target_x - master_x).abs() / 2.0;
        let center = (master_x + target_x) / 2.0;
        let rise = (radius * self.metrics.arc_height_scale)
            .min((axis_y - self.metrics.arc_top_inset).max(0.0));
        let path = list.path(
            FillRule::NonZero,
            [
                Verb::MoveTo(Pt {
                    x: master_x,
                    y: axis_y,
                }),
                Verb::QuadTo {
                    control: Pt {
                        x: center,
                        y: axis_y - rise,
                    },
                    to: Pt {
                        x: target_x,
                        y: axis_y,
                    },
                },
            ],
        );
        let (color, width) = if selected {
            (self.accent, self.metrics.selected_line_width)
        } else {
            (self.line_inner, self.metrics.line_width)
        };
        list.stroke_path(path, color, width);
    }

    fn marker(&self, list: &mut DrawListBuilder, x: f32, axis_y: f32, color: Rgba) {
        let size = self.metrics.marker_size;
        list.fill_rect(
            Rect {
                h: size,
                w: size,
                x: x - size / 2.0,
                y: axis_y - size / 2.0,
            },
            color,
        );
    }
}

#[derive(Clone, Copy)]
struct Scale {
    min: f32,
    span: f32,
    axis: Rect,
}

impl Scale {
    fn new(min: f32, max: f32, axis: Rect) -> Option<Self> {
        let span = max - min;
        (min.is_finite() && max.is_finite() && span > 0.0).then_some(Self { min, span, axis })
    }

    fn x(self, bpm: f32) -> f32 {
        let unit = ((bpm - self.min) / self.span).clamp(0.0, 1.0);
        self.axis.x + unit * self.axis.w
    }
}

const MAX_TICKS: f32 = 512.0;

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
    };

    #[kithara::test]
    fn portal_scale_maps_and_clamps_the_declared_tempo_range() {
        let scale = Scale::new(
            88.0,
            180.0,
            Rect {
                h: 1.0,
                w: 276.0,
                x: 12.0,
                y: 0.0,
            },
        )
        .unwrap();

        assert_eq!(scale.x(88.0), 12.0);
        assert_eq!(scale.x(180.0), 288.0);
        assert_eq!(scale.x(60.0), 12.0);
        assert_eq!(scale.x(200.0), 288.0);
    }

    /// The arc is the map's whole claim: a target at another tempo is drawn as
    /// a curve reaching it from the master, not as two unrelated marks.
    #[kithara::test]
    fn a_target_is_drawn_as_a_curve_from_the_master_tempo() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        let data = PortalMapData {
            master: 120.0,
            min: 60.0,
            max: 180.0,
            targets: vec![PortalTarget {
                bpm: 150.0,
                is_selected: false,
            }],
        };

        PortalMap::new(skin).paint(
            &mut list,
            &mut text,
            &data,
            Rect {
                h: 80.0,
                w: 300.0,
                x: 0.0,
                y: 0.0,
            },
        );

        let arcs: Vec<_> = list
            .finish()
            .commands()
            .iter()
            .filter_map(|command| match command {
                DrawCmd::Stroke {
                    geom: Geom::Path(path),
                    ..
                } => Some(path.verbs().to_vec()),
                _ => None,
            })
            .collect();
        let [verbs] = arcs.as_slice() else {
            panic!("one target must draw exactly one arc");
        };
        assert!(matches!(
            verbs.as_slice(),
            [Verb::MoveTo(from), Verb::QuadTo { to, .. }] if from.x < to.x
        ));
    }
}
