/// Channel letter -> session deck position. The studio addresses decks by
/// their channel letter, in control paths and in binding scopes alike, and the
/// letter is the deck's position in the session.
pub(in crate::gui) fn deck_index(letter: &str) -> Option<usize> {
    let [byte] = letter.as_bytes() else {
        return None;
    };
    byte.is_ascii_lowercase().then(|| usize::from(byte - b'a'))
}

pub(super) fn deck_letter(index: usize) -> Option<char> {
    let byte = u8::try_from(index).ok()?.checked_add(b'a')?;
    byte.is_ascii_lowercase().then(|| char::from(byte))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{deck_index, deck_letter};

    #[kithara::test]
    fn letters_are_session_positions() {
        assert_eq!(deck_index("a"), Some(0));
        assert_eq!(deck_index("d"), Some(3));
        assert_eq!(deck_index("A"), None);
        assert_eq!(deck_index("ab"), None);
        assert_eq!(deck_index(""), None);
    }

    #[kithara::test]
    fn positions_and_letters_are_one_mapping() {
        for (letter, index) in [("a", 0), ("d", 3), ("z", 25)] {
            assert_eq!(deck_index(letter), Some(index));
            assert_eq!(deck_letter(index), letter.chars().next());
        }
        assert_eq!(deck_letter(26), None);
        assert_eq!(deck_letter(usize::MAX), None);
    }
}
