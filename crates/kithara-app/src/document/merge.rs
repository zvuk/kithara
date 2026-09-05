use serde_yaml_ng::Value;

/// Lay `over` on top of `base`. Mappings merge key by key so a document names
/// only what it changes; every other value replaces what it covers, because a
/// list of tracks or of compression algorithms is one setting, not an append.
pub(crate) fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Mapping(base), Value::Mapping(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

#[cfg(test)]
mod tests {
    use serde_yaml_ng::Value;

    use super::merge;

    fn yaml(source: &str) -> Value {
        serde_yaml_ng::from_str(source).expect("valid yaml")
    }

    #[kithara::test(native, flash(false))]
    fn a_named_field_wins_and_its_siblings_survive() {
        let mut base = yaml("net:\n  compression: [gzip]\n  is_insecure: false\n");

        merge(&mut base, yaml("net:\n  is_insecure: true\n"));

        assert_eq!(base["net"]["is_insecure"], Value::from(true));
        assert_eq!(base["net"]["compression"][0], Value::from("gzip"));
    }

    #[kithara::test(native, flash(false))]
    fn a_sequence_is_replaced_whole_not_appended() {
        let mut base = yaml("playlist:\n  tracks: [a, b, c]\n");

        merge(&mut base, yaml("playlist:\n  tracks: [d]\n"));

        assert_eq!(base["playlist"]["tracks"], yaml("[d]"));
    }

    #[kithara::test(native, flash(false))]
    fn a_section_the_overlay_never_names_is_untouched() {
        let mut base = yaml("player:\n  crossfade_duration: 5.0\nplaylist:\n  tracks: [a]\n");

        merge(&mut base, yaml("playlist:\n  tracks: [b]\n"));

        assert_eq!(base["player"]["crossfade_duration"], Value::from(5.0));
    }

    #[kithara::test(native, flash(false))]
    fn merging_reaches_nested_mappings() {
        let mut base = yaml("a:\n  b:\n    c: 1\n    d: 2\n");

        merge(&mut base, yaml("a:\n  b:\n    c: 9\n"));

        assert_eq!(base["a"]["b"]["c"], Value::from(9));
        assert_eq!(base["a"]["b"]["d"], Value::from(2));
    }

    #[kithara::test(native, flash(false))]
    fn a_key_only_the_overlay_has_is_added() {
        let mut base = yaml("a:\n  b: 1\n");

        merge(&mut base, yaml("a:\n  c: 2\n"));

        assert_eq!(base["a"]["b"], Value::from(1));
        assert_eq!(base["a"]["c"], Value::from(2));
    }

    #[kithara::test(native, flash(false))]
    fn an_overlay_null_blanks_the_value_and_keeps_the_key() {
        let mut base = yaml("a: 1\nb: 2\n");

        merge(&mut base, yaml("a: null\n"));

        assert_eq!(base["a"], Value::Null);
        assert_eq!(base["b"], Value::from(2));
    }
}
