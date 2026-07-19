use iced::widget::{Text, text};

pub(crate) fn shaped_text<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    Text::new(content).shaping(text::Shaping::Advanced)
}
