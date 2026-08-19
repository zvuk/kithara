mod paint;
#[cfg(feature = "iced")]
mod program;
#[cfg(feature = "iced")]
mod widget;

pub(crate) use paint::text_input_layout;
#[cfg(feature = "iced")]
pub(crate) use widget::{search_input, sync_text_input};
