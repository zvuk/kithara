use iced::widget::{Text, text};

/// Creates text with advanced shaping enabled.
pub fn shaped_text<'a, T: text::IntoFragment<'a>>(content: T) -> Text<'a> {
    Text::new(content).shaping(text::Shaping::Advanced)
}
