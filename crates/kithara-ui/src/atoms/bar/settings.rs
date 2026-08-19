use crate::{
    atoms::{button::VisualState, design::quad::quad, icon::mark::Marked},
    draw::{DrawListBuilder, Rect, Rgba},
    render::{Mark, Skin},
    shaping::TextContext,
    skin::FrameSkin,
};

/// The global bar's own button: a framed panel with one mark centred in it.
pub(crate) struct Settings {
    frame: FrameSkin,
    hovered: Rgba,
    icon: Rgba,
    idle: Rgba,
    pressed: Rgba,
    size: f32,
    stroke: Rgba,
}

impl Settings {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.global_bar;
        Self {
            frame: metrics.settings_frame,
            hovered: skin.palette.bg_panel_2,
            icon: skin.palette.text_dim,
            idle: skin.palette.bg_panel,
            pressed: skin.palette.accent_soft,
            size: metrics.gear_size,
            stroke: skin.rgba(metrics.settings_frame.border),
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        mark: Mark,
        bounds: Rect,
        state: VisualState,
    ) {
        let fill = match state {
            VisualState::Idle => self.idle,
            VisualState::Hovered => self.hovered,
            VisualState::Pressed => self.pressed,
        };
        quad(list, bounds, self.frame, fill, self.stroke);
        Marked::new(mark, self.size).centred(list, text, bounds, self.icon);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Rect, Settings, TextContext, VisualState};
    use crate::{
        builtin,
        draw::DrawCmd,
        render::{Icon, Mark},
    };

    const BOUNDS: Rect = Rect {
        h: 32.0,
        w: 32.0,
        x: 6.0,
        y: 4.0,
    };

    fn gear() -> Mark {
        Icon::Gear
            .mark()
            .unwrap_or_else(|| panic!("the built-in gear must resolve to a mark"))
    }

    fn filled(state: VisualState) -> DrawCmd {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Settings::new(skin).paint(&mut list, &mut text, gear(), BOUNDS, state);
        list.finish()
            .commands()
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("a settings button must fill its box"))
    }

    /// The hand resting on the button and pressing it are three different
    /// pictures. A button that drew one of them for all three would leave the
    /// pointer with nothing to say it had landed.
    #[kithara::test]
    fn resting_on_the_button_and_pressing_it_each_fill_it_differently() {
        let idle = filled(VisualState::Idle);
        let hovered = filled(VisualState::Hovered);
        let pressed = filled(VisualState::Pressed);

        assert_ne!(idle, hovered);
        assert_ne!(hovered, pressed);
        assert_ne!(idle, pressed);
    }

    /// The mark sits in the middle of the box across as well as down, whatever
    /// the box it was given.
    #[kithara::test]
    fn the_mark_is_centred_in_the_box() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Settings::new(skin).paint(&mut list, &mut text, gear(), BOUNDS, VisualState::Idle);
        let list = list.finish();

        let placed = list
            .commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Text { transform, .. } => Some(transform.dx),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the gear must be shaped and placed"));
        let width = super::Marked::new(gear(), skin.global_bar.gear_size).width(&mut text);

        assert!((placed - (BOUNDS.x + (BOUNDS.w - width) / 2.0)).abs() < 0.001);
    }
}
