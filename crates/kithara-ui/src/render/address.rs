use crate::render::{ReadValue, Reads};

/// One owner in the address tree: it resolves its own children and reads its own value.
pub trait Node<'a> {
    fn child(&self, _segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        None
    }

    fn read(&self) -> Option<ReadValue<'a>> {
        None
    }
}

/// Values qualifying an address, spent by the owner of the instances they select.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scope<'s>(&'s str);

impl<'s> Scope<'s> {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&'s str> {
        self.0.split(',').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == key).then_some(value)
        })
    }
}

/// Answers the renderer's flat endpoint keys by walking the address tree.
pub struct Walk<'a> {
    root: Box<dyn Node<'a> + 'a>,
}

impl<'a> Walk<'a> {
    pub fn new<N: Node<'a> + 'a>(root: N) -> Self {
        Self {
            root: Box::new(root),
        }
    }
}

impl Reads for Walk<'_> {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (path, scope) = match endpoint.split_once('@') {
            Some((path, scope)) => (path, Scope(scope)),
            None => (endpoint, Scope::default()),
        };
        let mut segments = path.split('.');
        let mut node = self.root.child(segments.next()?, scope)?;
        for segment in segments {
            node = node.child(segment, scope)?;
        }
        node.read()
    }
}
