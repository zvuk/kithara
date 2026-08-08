use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    skin::KnobSkin,
    text::TextContext,
};

pub(crate) struct Knob {
    body_border: Rgba,
    body_fill: Rgba,
    indicator: Rgba,
    label: Rgba,
    metrics: KnobSkin,
    track: Rgba,
    value_color: Rgba,
}

impl Knob {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.knob;
        Self {
            body_border: skin.rgba(metrics.body_border),
            body_fill: skin.rgba(metrics.body_fill),
            indicator: skin.rgba(metrics.indicator_color),
            label: skin.rgba(metrics.label_text.color),
            metrics,
            track: skin.rgba(metrics.track_color),
            value_color: skin.rgba(metrics.value_color),
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        value: f32,
        label: Option<&str>,
        bounds: Rect,
    ) {
        let metrics = self.metrics;
        let (dial, caption) = self.rects(bounds);
        let side = dial.w.min(dial.h);
        let radius = side / 2.0;
        let center = Pt {
            x: dial.x + dial.w / 2.0,
            y: dial.y + dial.h / 2.0,
        };
        let angle = metrics.start_angle + metrics.sweep_angle * value;

        if radius > 0.0 {
            list.stroke_arc(
                center,
                radius,
                metrics.start_angle,
                metrics.start_angle + metrics.sweep_angle,
                Rgba {
                    a: metrics.track_alpha,
                    ..self.track
                },
                metrics.track_width,
            );
            list.stroke_arc(
                center,
                radius,
                metrics.neutral_angle,
                angle,
                self.value_color,
                metrics.track_width,
            );

            let body_radius = metrics.body_ratio * radius;
            list.fill_circle(center, body_radius, self.body_fill);
            list.stroke_circle(
                center,
                body_radius,
                self.body_border,
                metrics.body_border_width,
            );
            list.stroke_line(
                center,
                Pt {
                    x: center.x + angle.cos() * body_radius,
                    y: center.y + angle.sin() * body_radius,
                },
                self.indicator,
                metrics.indicator_width,
            );
        }

        if let Some(label) = label {
            let role = metrics.label_text;
            let run = text.shape(label, role, Some(caption.w));
            list.text(
                &run,
                label,
                Transform::translate(Pt {
                    x: caption.x + (caption.w - run.width()) / 2.0,
                    y: caption.y,
                }),
                self.label,
            );
        }
    }

    fn rects(&self, bounds: Rect) -> (Rect, Rect) {
        const DIAMETER_SCALE: f32 = 2.0;

        let metrics = self.metrics;
        let caption_height = metrics.label_height.min(bounds.h);
        let available = (bounds.h - caption_height).max(0.0);
        let gap = metrics.label_gap.min(available);
        let dial_height = available - gap;
        let inset = metrics
            .outer_inset
            .min(bounds.w / DIAMETER_SCALE)
            .min(dial_height / DIAMETER_SCALE);
        (
            Rect {
                h: dial_height - inset * DIAMETER_SCALE,
                w: bounds.w - inset * DIAMETER_SCALE,
                x: bounds.x + inset,
                y: bounds.y + inset,
            },
            Rect {
                h: caption_height,
                w: bounds.w,
                x: bounds.x,
                y: bounds.y + dial_height + gap,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, DrawList, Geom},
        ids::SourceUri,
    };

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Kind {
        FillCircle,
        StrokeCircle,
        StrokeArc,
        StrokeLine,
        Text,
        Other,
    }

    #[kithara::test]
    fn knob_emits_ordered_commands_with_optional_text() {
        let origin = SourceUri("knob.kskin.ron".to_owned());
        let skin = Skin::resolve(builtin::skin_doc().clone(), &origin).unwrap();
        let labelled = record(Some("GAIN"), &skin);
        let unlabelled = record(None, &skin);

        assert_eq!(
            kinds(&labelled),
            [
                Kind::StrokeArc,
                Kind::StrokeArc,
                Kind::FillCircle,
                Kind::StrokeCircle,
                Kind::StrokeLine,
                Kind::Text,
            ]
        );
        assert_eq!(
            kinds(&unlabelled),
            [
                Kind::StrokeArc,
                Kind::StrokeArc,
                Kind::FillCircle,
                Kind::StrokeCircle,
                Kind::StrokeLine,
            ]
        );
        assert_eq!(
            &labelled.commands()[..labelled.commands().len() - 1],
            unlabelled.commands(),
            "the dial recording must not depend on caption presence"
        );
        assert!(matches!(
            labelled.commands().last(),
            Some(DrawCmd::Text { content, .. }) if content == "GAIN"
        ));
    }

    #[kithara::test]
    fn caption_row_and_outer_inset_are_carved_from_the_bounds() {
        let origin = SourceUri("knob.kskin.ron".to_owned());
        let skin = Skin::resolve(builtin::skin_doc().clone(), &origin).unwrap();
        let bounds = Rect {
            h: 39.0,
            w: 28.0,
            x: 0.0,
            y: 0.0,
        };
        let metrics = skin.knob;
        let knob = Knob::new(&skin);
        let (dial, caption) = knob.rects(bounds);

        assert_eq!(
            dial.h + metrics.outer_inset * 2.0 + metrics.label_gap + caption.h,
            bounds.h
        );
        assert_eq!(caption.h, metrics.label_height);
        assert!(caption.y >= dial.y + dial.h);
    }

    fn record(label: Option<&str>, skin: &Skin) -> DrawList {
        const BOUNDS: Rect = Rect {
            h: 39.0,
            w: 28.0,
            x: 0.0,
            y: 0.0,
        };

        let knob = Knob::new(skin);
        let mut list = DrawListBuilder::default();
        knob.paint(
            &mut list,
            &mut TextContext::new().unwrap(),
            0.25,
            label,
            BOUNDS,
        );
        list.finish()
    }

    fn kinds(list: &DrawList) -> Vec<Kind> {
        let commands = list.commands();
        commands
            .iter()
            .map(|command| match command {
                DrawCmd::Fill {
                    geom: Geom::Circle { .. },
                    ..
                } => Kind::FillCircle,
                DrawCmd::Stroke {
                    geom: Geom::Circle { .. },
                    ..
                } => Kind::StrokeCircle,
                DrawCmd::Stroke {
                    geom: Geom::Arc { .. },
                    ..
                } => Kind::StrokeArc,
                DrawCmd::Stroke {
                    geom: Geom::Line { .. },
                    ..
                } => Kind::StrokeLine,
                DrawCmd::Text { .. } => Kind::Text,
                _ => Kind::Other,
            })
            .collect()
    }
}
