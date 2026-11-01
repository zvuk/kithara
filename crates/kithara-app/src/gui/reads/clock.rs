use kithara::ui::render::{Node, ReadValue, Scope};

use super::value::Value;

/// The host tempo the bar draws and steps.
#[derive(Clone, Copy)]
pub(super) struct ClockNode<'a> {
    bpm: &'a str,
    tempo: f64,
}

impl<'a> ClockNode<'a> {
    pub(super) const fn new(bpm: &'a str, tempo: f64) -> Self {
        Self { bpm, tempo }
    }
}

impl<'a> Node<'a> for ClockNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "bpm" => ReadValue::Text(self.bpm),
            "tempo" => ReadValue::Scalar(self.tempo),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
