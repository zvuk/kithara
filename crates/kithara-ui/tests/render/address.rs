use kithara_test_utils::kithara;
use kithara_ui::render::{Node, ReadValue, Reads, Scope, Walk};

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
        const TEMPOS: [&str; 2] = ["120.0", "128.0"];

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
