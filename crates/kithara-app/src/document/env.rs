use std::{collections::BTreeSet, fmt};

use serde_yaml_ng::Value;

/// Names a document referenced that no source resolved, each with the position
/// it sits at. Carries every pair, so one startup reports the whole gap instead
/// of the first hole in it. Expansion runs over the merged tree, so the position
/// is what tells an operator which of the two documents to fix.
#[derive(Debug)]
#[non_exhaustive]
pub struct MissingEnv(BTreeSet<(String, String)>);

impl fmt::Display for MissingEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unset environment variables referenced by the configuration:")?;
        for (name, path) in &self.0 {
            write!(f, "\n  {name} ({path})")?;
        }
        Ok(())
    }
}

impl std::error::Error for MissingEnv {}

/// Replace every `$VAR` and `${VAR}` reference in `value` with what `lookup`
/// resolves, walking the whole tree.
///
/// An empty value counts as unresolved: a blank key reaches the key server as a
/// rejected request, which is harder to read than a refusal to start.
///
/// # Errors
/// Returns every name that resolved to nothing, with the position it sits at.
pub(crate) fn expand(
    value: &mut Value,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), MissingEnv> {
    let mut missing = BTreeSet::new();
    walk(value, "", lookup, &mut missing);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MissingEnv(missing))
    }
}

/// `path` spells the position the way `build.rs::collect_refs` does -- a dot
/// before a mapping key, `[i]` for a sequence index -- so a reference reads the
/// same whether the build refused it or the startup did.
fn walk(
    value: &mut Value,
    path: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    missing: &mut BTreeSet<(String, String)>,
) {
    match value {
        Value::String(text) => {
            if let Some(rendered) = render(text, path, lookup, missing) {
                *text = rendered;
            }
        }
        Value::Sequence(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                walk(item, &format!("{path}[{index}]"), lookup, missing);
            }
        }
        Value::Mapping(entries) => {
            for (key, entry) in entries.iter_mut() {
                let key = key.as_str().unwrap_or("?");
                let child = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                walk(entry, &child, lookup, missing);
            }
        }
        _ => {}
    }
}

/// `None` when the string holds no reference, or when one of its references is
/// unresolved -- the unresolved names land in `missing` and the original text
/// stays in place for the error path to report against.
fn render(
    text: &str,
    path: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    missing: &mut BTreeSet<(String, String)>,
) -> Option<String> {
    if !text.contains("${") {
        let name = text.strip_prefix('$')?;
        let found = resolve(name, lookup);
        if found.is_none() {
            missing.insert((name.to_string(), path.to_string()));
        }
        return found;
    }

    let mut rendered = String::new();
    let mut rest = text;
    let mut resolved = true;
    while let Some(start) = rest.find("${") {
        let (before, tail) = rest.split_at(start);
        rendered.push_str(before);
        let tail = &tail[2..];
        let Some(end) = tail.find('}') else {
            rendered.push_str("${");
            rest = tail;
            continue;
        };
        let name = &tail[..end];
        if let Some(value) = resolve(name, lookup) {
            rendered.push_str(&value);
        } else {
            missing.insert((name.to_string(), path.to_string()));
            resolved = false;
        }
        rest = &tail[end + 1..];
    }
    rendered.push_str(rest);
    resolved.then_some(rendered)
}

fn resolve(name: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    lookup(name).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_yaml_ng::Value;

    use super::expand;

    fn lookup(pairs: &HashMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
        move |name| pairs.get(name).cloned()
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn whole_string_reference_becomes_the_value() {
        let pairs = env(&[("KITHARA_TOKEN", "secret")]);
        let mut value: Value = serde_yaml_ng::from_str("key: $KITHARA_TOKEN").expect("valid yaml");

        expand(&mut value, &lookup(&pairs)).expect("every reference resolves");

        assert_eq!(value["key"], Value::from("secret"));
    }

    #[kithara::test(native, flash(false))]
    fn embedded_reference_is_substituted_in_place() {
        let pairs = env(&[("KITHARA_HOST", "cdn.example")]);
        let mut value: Value = serde_yaml_ng::from_str("key: https://${KITHARA_HOST}/master.m3u8")
            .expect("valid yaml");

        expand(&mut value, &lookup(&pairs)).expect("every reference resolves");

        assert_eq!(value["key"], Value::from("https://cdn.example/master.m3u8"));
    }

    #[kithara::test(native, flash(false))]
    fn references_resolve_at_any_depth() {
        let pairs = env(&[("KITHARA_TOKEN", "secret")]);
        let mut value: Value = serde_yaml_ng::from_str(
            "drm:\n  providers:\n    - headers:\n        X-Auth-Token: $KITHARA_TOKEN\n",
        )
        .expect("valid yaml");

        expand(&mut value, &lookup(&pairs)).expect("every reference resolves");

        assert_eq!(
            value["drm"]["providers"][0]["headers"]["X-Auth-Token"],
            Value::from("secret")
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_literal_dollar_inside_a_word_is_left_alone() {
        let pairs = env(&[]);
        let mut value: Value = serde_yaml_ng::from_str("key: costs 5$ today").expect("valid yaml");

        expand(&mut value, &lookup(&pairs)).expect("no reference to resolve");

        assert_eq!(value["key"], Value::from("costs 5$ today"));
    }

    #[kithara::test(native, flash(false))]
    fn every_missing_name_is_reported_not_just_the_first() {
        let pairs = env(&[]);
        let mut value: Value =
            serde_yaml_ng::from_str("a: $KITHARA_ONE\nb: ${KITHARA_TWO}\n").expect("valid yaml");

        let missing = expand(&mut value, &lookup(&pairs)).expect_err("both names are unset");

        let report = missing.to_string();
        assert!(report.contains("KITHARA_ONE"), "{report}");
        assert!(report.contains("KITHARA_TWO"), "{report}");
    }

    #[kithara::test(native, flash(false))]
    fn a_missing_name_is_reported_with_the_position_it_sits_at() {
        let pairs = env(&[]);
        let mut value: Value = serde_yaml_ng::from_str(
            "drm:\n  providers:\n    - cipher_key: $KITHARA_DEFINITELY_UNSET\n",
        )
        .expect("valid yaml");

        let missing = expand(&mut value, &lookup(&pairs)).expect_err("the name is unset");

        let report = missing.to_string();
        assert!(report.contains("drm.providers[0].cipher_key"), "{report}");
    }

    #[kithara::test(native, flash(false))]
    fn an_empty_value_counts_as_missing() {
        let pairs = env(&[("KITHARA_TOKEN", "")]);
        let mut value: Value = serde_yaml_ng::from_str("key: $KITHARA_TOKEN").expect("valid yaml");

        let missing =
            expand(&mut value, &lookup(&pairs)).expect_err("an empty value is not a value");

        assert!(missing.to_string().contains("KITHARA_TOKEN"));
    }
}
