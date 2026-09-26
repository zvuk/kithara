use crate::{
    atoms::{
        button::VisualState,
        design::quad::{center_y, quad},
        painter::ControlPainter,
    },
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    shaping::TextContext,
    skin::{ColorRole, FrameSkin, TextRoleSkin},
    solve::{Length, Size},
};

/// The word a module's footer carries, in the one role both hosts shape it in.
pub(crate) fn footer_role(skin: &Skin) -> TextRoleSkin {
    skin.chrome.footer_text
}

/// A chip or a title in a module header: a filled box around a shaped label.
///
/// Every number the skin settles is resolved when the label is built, so the
/// two hosts draw the same box from the same figures instead of each reading
/// the skin its own way.
#[derive(Clone, PartialEq, kithara_derive::Retained)]
pub(crate) struct ChromeLabel {
    frame: FrameSkin,
    background: Rgba,
    border: Rgba,
    color: Rgba,
    role: TextRoleSkin,
    padding_x: f32,
}

impl ChromeLabel {
    fn new(skin: &Skin, background: ColorRole, frame: FrameSkin, role: TextRoleSkin) -> Self {
        Self {
            background: skin.rgba(background),
            border: skin.rgba(frame.border),
            color: skin.rgba(role.color),
            frame,
            padding_x: skin.chrome.chip_pad,
            role,
        }
    }

    /// The accent box that names the module.
    pub(crate) fn chip(skin: &Skin) -> Self {
        let metrics = skin.chrome;
        Self::new(
            skin,
            metrics.chip_background,
            metrics.chip_frame,
            metrics.chip_text,
        )
    }

    /// The box the label asks for: its run, unwrapped, and the padding either
    /// side of it.
    pub(crate) fn intrinsic(&self, text: &mut TextContext, content: &str) -> (f32, f32) {
        let run = text.shape(content, self.role, None);
        (run.width() + self.padding_x * 2.0, run.height())
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        content: &str,
        bounds: Rect,
    ) {
        quad(list, bounds, self.frame, self.background, self.border);
        if content.is_empty() {
            return;
        }
        let max_width = (bounds.w - self.padding_x * 2.0).max(0.0);
        let run = text.shape(content, self.role, Some(max_width));
        list.text(
            &run,
            content,
            Transform::translate(Pt {
                x: bounds.x + self.padding_x,
                y: center_y(bounds, &run),
            }),
            self.color,
        );
    }

    /// The title beside the chip.
    pub(crate) fn title(skin: &Skin) -> Self {
        let metrics = skin.chrome;
        Self::new(
            skin,
            metrics.title_background,
            metrics.title_frame,
            metrics.title_text,
        )
    }
}

/// A header label is a word in a box: it asks for the width its own run needs
/// and takes the header's height.
impl ControlPainter for ChromeLabel {
    type Data = String;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }

    fn length(&self, _text: &mut TextContext, _data: &Self::Data) -> Size<Length> {
        Size::new(Length::Shrink, Length::Fill)
    }

    fn measure(&self, text: &mut TextContext, data: &Self::Data) -> Size {
        let (width, height) = self.intrinsic(text, data);
        Size::new(width, height)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, draw::DrawCmd};

    mod consts {
        use super::*;

        /// A header the whole width of a module, and the cell the chevron sits in
        /// at the end of it.
        pub(super) const HEADER: Rect = Rect {
            h: 26.0,
            w: 200.0,
            x: 12.0,
            y: 4.0,
        };
    }

    fn drawn(paint: impl FnOnce(&mut DrawListBuilder, &mut TextContext)) -> Vec<DrawCmd> {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        paint(&mut list, &mut text);
        list.finish().commands().to_vec()
    }

    #[kithara::test]
    fn a_chip_draws_its_box_before_its_word() {
        let skin = builtin::skin();
        let commands = drawn(|list, text| {
            ChromeLabel::chip(skin).paint(list, text, "FX", consts::HEADER);
        });

        assert!(
            matches!(
                commands.as_slice(),
                [DrawCmd::Fill { .. }, DrawCmd::Text { content, .. }] if content == "FX"
            ),
            "a chip must fill its box and then set its word in it: {commands:?}"
        );
    }

    #[kithara::test]
    fn a_chip_sets_its_word_in_the_colour_the_skin_names() {
        let skin = builtin::skin();
        let commands = drawn(|list, text| {
            ChromeLabel::chip(skin).paint(list, text, "FX", consts::HEADER);
        });

        let Some(DrawCmd::Text { color, .. }) = commands.last() else {
            panic!("a chip must set a word: {commands:?}");
        };
        assert_eq!(*color, skin.rgba(skin.chrome.chip_text.color));
    }

    #[kithara::test]
    fn a_label_asks_for_the_padding_either_side_of_its_word() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let label = ChromeLabel::title(skin);
        let run = text.shape("DECK", label.role, None);
        let word = run.width();

        let (width, _) = label.intrinsic(&mut text, "DECK");

        assert!(
            (width - (word + skin.chrome.chip_pad * 2.0)).abs() < 0.001,
            "a title of {word} px asked for {width} px"
        );
    }

    #[kithara::test]
    fn a_label_asks_for_the_height_of_its_own_run() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let label = ChromeLabel::title(skin);
        let run = text.shape("DECK", label.role, None);
        let line = run.height();

        let (_, height) = label.intrinsic(&mut text, "DECK");

        assert!(
            (height - line).abs() < 0.001,
            "a run of {line} px measured {height} px"
        );
    }
}
