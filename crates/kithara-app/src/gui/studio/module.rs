use iced::{
    Background, Color, Element, Length, Padding, Theme,
    widget::{container, container::Style as ContainerStyle},
};

/// The single block-container primitive every studio module goes through.
#[derive(Clone, Copy)]
pub(super) struct Module {
    bg: Option<Color>,
    pad: Padding,
    height: Height,
}

#[derive(Clone, Copy)]
enum Height {
    Shrink,
    Fill,
    Fixed(f32),
}

impl Module {
    /// Transparent, shrink-height module with no padding.
    pub(super) fn new() -> Self {
        Self {
            bg: None,
            pad: Padding::ZERO,
            height: Height::Shrink,
        }
    }

    /// Fills the module background; transparent when left unset.
    pub(super) fn bg(mut self, bg: Color) -> Self {
        self.bg = Some(bg);
        self
    }

    /// Inner padding, normally one of the `studio_space` tokens.
    pub(super) fn pad(mut self, pad: impl Into<Padding>) -> Self {
        self.pad = pad.into();
        self
    }

    /// Stretches the module over the available vertical space.
    pub(super) fn fill_height(mut self) -> Self {
        self.height = Height::Fill;
        self
    }

    /// Pins the module to a fixed height and centers content in it.
    pub(super) fn height(mut self, height: f32) -> Self {
        self.height = Height::Fixed(height);
        self
    }

    /// Builds the styled container around `content`.
    pub(super) fn wrap<'a, M: 'a>(self, content: impl Into<Element<'a, M>>) -> Element<'a, M> {
        let Self { bg, pad, height } = self;
        let shell = container(content).width(Length::Fill).padding(pad);
        let shell = match height {
            Height::Shrink => shell,
            Height::Fill => shell.height(Length::Fill),
            Height::Fixed(h) => shell.center_y(Length::Fixed(h)),
        };
        shell
            .style(move |_theme: &Theme| ContainerStyle {
                background: bg.map(Background::Color),
                ..ContainerStyle::default()
            })
            .into()
    }
}
