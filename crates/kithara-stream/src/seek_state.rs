#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use crate::ActivityWriter;

    #[kithara::test]
    fn playing_defaults_to_false() {
        let s = ActivityWriter::new().reader();
        assert!(!s.is_playing());
    }

    #[kithara::test]
    fn set_playing_true_is_visible_across_arc_clones() {
        let mut writer = ActivityWriter::new();
        let s = Arc::new(writer.reader());
        let clone = Arc::clone(&s);
        writer.set_playing(true);
        assert!(clone.is_playing());
        writer.set_playing(false);
        assert!(!s.is_playing());
    }

    #[kithara::test]
    fn set_playing_idempotent() {
        let mut writer = ActivityWriter::new();
        let s = writer.reader();
        writer.set_playing(true);
        writer.set_playing(true);
        assert!(s.is_playing());
        writer.set_playing(false);
        writer.set_playing(false);
        assert!(!s.is_playing());
    }
}
