use kithara_ui::render::{Node, ReadValue, Scope};

use super::value::Value;

/// What the bar cell reads about the live stream.
#[derive(Clone, Copy)]
pub(super) struct BroadcastNode<'a> {
    on_air: bool,
    url: &'a str,
}

impl<'a> BroadcastNode<'a> {
    pub(super) const fn new(on_air: bool, url: &'a str) -> Self {
        Self { on_air, url }
    }
}

impl<'a> Node<'a> for BroadcastNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "on_air" => ReadValue::Bool(self.on_air),
            "url" => ReadValue::Text(self.url),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
