use crate::{
    draw::{DrawListBuilder, Pt, Rect, Transform},
    render::Skin,
    shaping::TextContext,
    skin::TextRoleSkin,
};

pub(crate) struct Text<'data, 'skin> {
    content: &'data str,
    padding_x: f32,
    role: TextRoleSkin,
    skin: &'skin Skin,
}

impl<'data, 'skin> Text<'data, 'skin> {
    pub(crate) const fn new(
        content: &'data str,
        role: TextRoleSkin,
        padding_x: f32,
        skin: &'skin Skin,
    ) -> Self {
        Self {
            content,
            padding_x,
            role,
            skin,
        }
    }

    /// Returns the intrinsic width and height in logical pixels.
    ///
    /// Shaped unbounded, because this is what the control asks its parent for.
    /// [`Self::paint`] shapes against what it was given instead, so a squeezed
    /// box breaks lines rather than overflowing.
    pub(crate) fn measure(&self, text: &mut TextContext) -> (f32, f32) {
        let run = text.shape(self.content, self.role, None);
        (run.width() + self.padding_x * 2.0, run.height())
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, text: &mut TextContext, bounds: Rect) {
        if self.content.is_empty() {
            return;
        }

        let max_width = (bounds.w - self.padding_x * 2.0).max(0.0);
        let run = text.shape(self.content, self.role, Some(max_width));
        list.text(
            &run,
            self.content,
            Transform::translate(Pt {
                x: bounds.x + self.padding_x,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            self.skin.rgba(self.role.color),
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, draw::DrawCmd, ids::SourceUri};

    #[kithara::test]
    fn tracking_widens_the_measured_run() {
        let origin = SourceUri("text.kskin.ron".to_owned());
        let skin =
            Skin::resolve(builtin::skin_doc().clone(), builtin::text_doc(), &origin).unwrap();
        let role = skin.text.body;
        let mut text = TextContext::from(skin.text_resources());
        let plain = Text::new(
            "TRACKING",
            TextRoleSkin {
                spacing: 0.0,
                ..role
            },
            0.0,
            &skin,
        )
        .measure(&mut text);
        let tracked = Text::new(
            "TRACKING",
            TextRoleSkin {
                spacing: 0.12,
                ..role
            },
            0.0,
            &skin,
        )
        .measure(&mut text);

        assert!(tracked.0 > plain.0);
    }

    #[kithara::test]
    fn an_empty_string_emits_no_command() {
        let origin = SourceUri("text.kskin.ron".to_owned());
        let skin =
            Skin::resolve(builtin::skin_doc().clone(), builtin::text_doc(), &origin).unwrap();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Text::new("", skin.text.body, 0.0, &skin).paint(
            &mut list,
            &mut text,
            Rect {
                h: 40.0,
                w: 120.0,
                x: 4.0,
                y: 8.0,
            },
        );

        assert!(list.finish().commands().is_empty());
    }

    #[kithara::test]
    fn text_command_stays_inside_bounds_vertically() {
        let origin = SourceUri("text.kskin.ron".to_owned());
        let skin =
            Skin::resolve(builtin::skin_doc().clone(), builtin::text_doc(), &origin).unwrap();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        let bounds = Rect {
            h: 40.0,
            w: 120.0,
            x: 4.0,
            y: 8.0,
        };
        Text::new("TEXT", skin.text.body, 6.0, &skin).paint(&mut list, &mut text, bounds);
        let list = list.finish();

        let [DrawCmd::Text { run, transform, .. }] = list.commands() else {
            panic!("expected one text command");
        };
        assert!(transform.dy >= bounds.y);
        assert!(transform.dy + run.height() <= bounds.y + bounds.h);
    }
}
