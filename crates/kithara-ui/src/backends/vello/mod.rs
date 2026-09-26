mod backend;
#[cfg(test)]
mod tests;

pub(super) use self::backend::has_system_text;
pub use self::backend::{VelloBackend, paint_color};
