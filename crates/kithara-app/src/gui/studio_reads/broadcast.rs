use kithara_ui::render::{Node, ReadValue, Scope};

use super::value::Value;

/// What the bar cell reads about the live stream. A build without the
/// `broadcast` feature hides the cell instead of showing one that does nothing.
#[derive(Clone, Copy)]
pub(super) struct BroadcastNode<'a> {
    hidden: bool,
    on_air: bool,
    url: &'a str,
}

impl<'a> BroadcastNode<'a> {
    /// A build that cannot broadcast hides the cell rather than showing one
    /// that never lights.
    const AVAILABLE: bool = cfg!(feature = "broadcast");

    pub(super) const fn new(on_air: bool, url: &'a str) -> Self {
        Self {
            hidden: !Self::AVAILABLE,
            on_air,
            url,
        }
    }
}

impl<'a> Node<'a> for BroadcastNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "hidden" => ReadValue::Bool(self.hidden),
            "on_air" => ReadValue::Bool(self.on_air),
            "url" => ReadValue::Text(self.url),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
