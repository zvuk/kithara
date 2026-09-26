use serde::{Deserialize, Serialize};

/// A weight a text style asks of its family.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FontWeight {
    Normal,
    Medium,
    Semibold,
    Bold,
}

/// A family a text style is set in.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FontFamily {
    Display,
    Sans,
    Mono,
}

/// Everything shaping needs to set one run of text.
///
/// `spacing` is letter tracking as a fraction of `size`; it travels with the
/// face so a caller cannot shape text and drop the tracking its style declared.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub font: FontFamily,
    pub weight: FontWeight,
    pub size: f32,
    pub spacing: f32,
}
