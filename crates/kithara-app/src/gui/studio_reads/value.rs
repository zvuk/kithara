use kithara_ui::render::{Node, ReadValue};

pub(super) struct Value<'a>(pub(super) ReadValue<'a>);

impl<'a> Node<'a> for Value<'a> {
    fn read(&self) -> Option<ReadValue<'a>> {
        Some(self.0)
    }
}
