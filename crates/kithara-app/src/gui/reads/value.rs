use kithara::ui::render::{Node, ReadValue};

macro_rules! impl_child_node {
    ($node:ty, |$this:ident, $segment:ident, $scope:ident| $body:block) => {
        impl<'a> Node<'a> for $node {
            fn child(
                &self,
                segment: &str,
                scope: kithara::ui::render::Scope<'_>,
            ) -> Option<Box<dyn Node<'a> + 'a>> {
                let $this = self;
                let $segment = segment;
                let $scope = scope;
                $body
            }
        }
    };
}

pub(super) use impl_child_node;

pub(super) struct Value<'a>(pub(super) ReadValue<'a>);

impl<'a> Node<'a> for Value<'a> {
    fn read(&self) -> Option<ReadValue<'a>> {
        Some(self.0)
    }
}
