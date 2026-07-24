/// Channel letter → session deck position. The studio addresses decks by
/// their channel letter, in control paths and in binding scopes alike, and the
/// letter is the deck's position in the session.
pub(super) fn deck_index(letter: &str) -> Option<usize> {
    let [byte] = letter.as_bytes() else {
        return None;
    };
    byte.is_ascii_lowercase().then(|| usize::from(byte - b'a'))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::deck_index;

    #[kithara::test]
    fn letters_are_session_positions() {
        assert_eq!(deck_index("a"), Some(0));
        assert_eq!(deck_index("d"), Some(3));
        assert_eq!(deck_index("A"), None);
        assert_eq!(deck_index("ab"), None);
        assert_eq!(deck_index(""), None);
    }
}
