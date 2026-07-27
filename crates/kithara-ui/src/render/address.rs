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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const TEMPOS: [&str; 2] = ["120.0", "128.0"];

    struct Root;
    struct Decks;
    struct Playback(usize);
    struct Tempo(usize);

    impl<'a> Node<'a> for Root {
        fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
            (segment == "deck").then(|| Box::new(Decks) as Box<dyn Node<'a>>)
        }
    }

    impl<'a> Node<'a> for Decks {
        fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
            let letter = scope.get("deck")?;
            let index = usize::from(letter.as_bytes().first()?.checked_sub(b'a')?);
            (segment == "playback").then(|| Box::new(Playback(index)) as Box<dyn Node<'a>>)
        }
    }

    impl<'a> Node<'a> for Playback {
        fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
            (segment == "tempo").then(|| Box::new(Tempo(self.0)) as Box<dyn Node<'a>>)
        }
    }

    impl<'a> Node<'a> for Tempo {
        fn read(&self) -> Option<ReadValue<'a>> {
            TEMPOS.get(self.0).map(|tempo| ReadValue::Text(tempo))
        }
    }

    #[kithara::test]
    fn scope_selects_the_instance_the_path_does_not() {
        let walk = Walk::new(Root);

        assert_eq!(
            walk.get("deck.playback.tempo@deck=a"),
            Some(ReadValue::Text("120.0"))
        );
        assert_eq!(
            walk.get("deck.playback.tempo@deck=b"),
            Some(ReadValue::Text("128.0"))
        );
        assert_eq!(walk.get("deck.playback.tempo"), None);
    }

    #[kithara::test]
    fn an_address_no_owner_claims_reads_nothing() {
        let walk = Walk::new(Root);

        assert_eq!(walk.get("deck.playback.pitch@deck=a"), None);
        assert_eq!(walk.get("mixer.trim@deck=a"), None);
        assert_eq!(walk.get("deck.playback@deck=a"), None);
    }
}
