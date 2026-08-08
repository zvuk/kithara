use crate::{
    atoms::design::quad::{center_y, quad},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::ScalarFormat,
    render::Skin,
    skin::{FontFamily, TelemetrySkin, TextRoleSkin},
    solve::{Length, Size},
    text::TextContext,
};

/// One formatted number, centred in its box and framed only when the document
/// asked for it.
pub(crate) struct Telemetry {
    format: ScalarFormat,
    framed: bool,
    inset: Rgba,
    metrics: TelemetrySkin,
    role: TextRoleSkin,
    stroke: Rgba,
    text: Rgba,
}

impl Telemetry {
    pub(crate) fn new(format: ScalarFormat, framed: bool, skin: &Skin) -> Self {
        let metrics = skin.telemetry;
        Self {
            format,
            framed,
            inset: skin.palette.bg_inset,
            metrics,
            role: TextRoleSkin {
                color: crate::skin::ColorRole::Text,
                font: FontFamily::Mono,
                size: metrics.text.size,
                spacing: 0.0,
                weight: metrics.text.weight,
            },
            stroke: skin.rgba(metrics.frame.border),
            text: skin.palette.text,
        }
    }

    /// The number as it is shown. A percentage is padded to a fixed width so a
    /// changing reading does not shuffle the row it sits in.
    pub(crate) fn format(&self, value: f64) -> String {
        if self.format == ScalarFormat::Percent {
            format!(
                "{value:>width$.precision$}%",
                value = value * self.metrics.percent_scale,
                width = self.metrics.percent_width,
                precision = self.metrics.percent_precision,
            )
        } else {
            format!(
                "{value:.precision$}",
                precision = self.metrics.scalar_precision,
            )
        }
    }

    /// A framed reading fills its row; a bare one is as wide as its digits.
    pub(crate) const fn declared(&self) -> Size<Length> {
        if self.framed {
            Size::new(Length::Fill, Length::Fill)
        } else {
            Size::new(Length::Shrink, Length::Fill)
        }
    }

    pub(crate) fn intrinsic_width(&self, text: &mut TextContext, reading: &str) -> f32 {
        text.shape(reading, self.role, None).width()
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        reading: &str,
        bounds: Rect,
    ) {
        if self.framed {
            quad(list, bounds, self.metrics.frame, self.inset, self.stroke);
        }
        let run = text.shape(reading, self.role, None);
        list.text(
            &run,
            reading,
            Transform::translate(Pt {
                x: bounds.x + (bounds.w - run.width()) / 2.0,
                y: center_y(bounds, &run),
            }),
            self.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{ScalarFormat, Telemetry};
    use crate::{builtin, solve::Length};

    /// A percentage is padded so the row does not shuffle as the reading
    /// changes; a plain scalar is not.
    #[kithara::test]
    fn a_percentage_keeps_one_width_across_readings() {
        let skin = builtin::skin();
        let percent = Telemetry::new(ScalarFormat::Percent, false, skin);

        assert_eq!(percent.format(0.05).len(), percent.format(1.0).len());
    }

    #[kithara::test]
    fn a_plain_scalar_is_shown_to_the_precision_its_skin_names() {
        let skin = builtin::skin();
        let plain = Telemetry::new(ScalarFormat::Default, false, skin);
        let dot = plain.format(0.5).find('.').unwrap_or_default();

        assert_eq!(
            plain.format(0.5).len() - dot - 1,
            skin.telemetry.scalar_precision
        );
    }

    /// A bare reading is as wide as its digits, so it must ask to shrink; a
    /// framed one owns its row.
    #[kithara::test]
    fn only_a_bare_reading_asks_to_shrink() {
        let skin = builtin::skin();

        assert_eq!(
            Telemetry::new(ScalarFormat::Default, false, skin)
                .declared()
                .width,
            Length::Shrink
        );
        assert_eq!(
            Telemetry::new(ScalarFormat::Default, true, skin)
                .declared()
                .width,
            Length::Fill
        );
    }
}
