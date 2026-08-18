use kithara_ui::render::{Node, ReadValue, Scope};

use super::value::Value;

/// What the bar cell reads about the live stream.
#[derive(Clone, Copy)]
pub(super) struct BroadcastNode<'a> {
    on_air: bool,
    url: &'a str,
    available: bool,
}

impl<'a> BroadcastNode<'a> {
    pub(super) const fn new(on_air: bool, url: &'a str, available: bool) -> Self {
        Self {
            on_air,
            url,
            available,
        }
    }
}

impl<'a> Node<'a> for BroadcastNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "on_air" => ReadValue::Bool(self.on_air),
            "url" => ReadValue::Text(self.url),
            "hidden" => ReadValue::Bool(!self.available),
            "hint" => ReadValue::Text(if self.on_air { "AUDIO LIVE" } else { "OFF" }),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
