use crate::deck::EqMode;

/// Channel letter -> session deck position. The app addresses decks by
/// their channel letter, in control paths and in binding scopes alike, and the
/// letter is the deck's position in the session.
pub(in crate::gui) fn deck_index(letter: &str) -> Option<usize> {
    let [byte] = letter.as_bytes() else {
        return None;
    };
    byte.is_ascii_lowercase().then(|| usize::from(byte - b'a'))
}

/// The deck the micro bar drives. Its addresses carry no letter, so the
/// document and the host name it in one place each and a unit test holds the
/// two together.
pub(in crate::gui) const MICRO_DECK: &str = "a";

pub(super) fn deck_letter(index: usize) -> Option<char> {
    let byte = u8::try_from(index).ok()?.checked_add(b'a')?;
    byte.is_ascii_lowercase().then(|| char::from(byte))
}

/// Knob id -> band index in the mode that draws it. The banks share a strip,
/// so each knob carries its band count and only the drawn bank answers.
pub(in crate::gui) fn eq_band(mode: EqMode, control: &str) -> Option<usize> {
    let band = match (mode, control) {
        (EqMode::ThreeBand, "low-3") | (EqMode::FourBand, "low-4") => "low",
        (EqMode::ThreeBand, "mid-3") => "mid",
        (EqMode::ThreeBand, "high-3") | (EqMode::FourBand, "high-4") => "high",
        (EqMode::FourBand, "low-mid-4") => "low_mid",
        (EqMode::FourBand, "high-mid-4") => "high_mid",
        _ => return None,
    };
    mode.band(band)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{EqMode, deck_index, deck_letter, eq_band};

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

    #[kithara::test]
    fn eq_controls_only_address_bands_in_the_visible_mode() {
        assert_eq!(eq_band(EqMode::ThreeBand, "mid-3"), Some(1));
        assert_eq!(eq_band(EqMode::ThreeBand, "high-mid-4"), None);
        assert_eq!(eq_band(EqMode::FourBand, "high-mid-4"), Some(2));
        assert_eq!(eq_band(EqMode::FourBand, "mid-3"), None);
    }
}
