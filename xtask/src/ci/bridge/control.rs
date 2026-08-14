use std::path::Path;

/// What a change to a CI control path does to the trusted judge.
///
/// A pull request cannot be tested with its own CI configuration and still be
/// trusted, because a configuration is exactly what decides whether a run is
/// green. But the danger is not new entries — a lane added beside the existing
/// ones weakens nothing. It is existing entries that matter: an `allow_failure`
/// added to a job that judges, a lane dropped from `needs`, a pin moved, a
/// filter narrowed. So the boundary is not which file changed but whether the
/// change left everything that was already there untouched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ControlChange {
    /// Only new entries. The judge keeps every promise it made before.
    Additive,
    /// Something that already existed changed or disappeared.
    Weakening,
}

/// Classify one control path from its content on each side. `None` means the
/// file is absent there.
pub(super) fn classify(path: &str, base: Option<&str>, head: Option<&str>) -> ControlChange {
    match (base, head) {
        (None, Some(_)) => ControlChange::Additive,
        (_, None) => ControlChange::Weakening,
        (Some(base), Some(head)) if base == head => ControlChange::Additive,
        (Some(base), Some(head)) => match Format::of(path) {
            Format::Toml => toml_change(base, head),
            Format::Yaml => yaml_change(base, head),
            Format::Opaque => line_change(base, head),
        },
    }
}

enum Format {
    Toml,
    Yaml,
    Opaque,
}

impl Format {
    fn of(path: &str) -> Self {
        match Path::new(path).extension().and_then(|ext| ext.to_str()) {
            Some("toml") => Self::Toml,
            Some("yml" | "yaml") => Self::Yaml,
            _ => Self::Opaque,
        }
    }
}

/// A parse failure falls through to the line rule rather than to a verdict:
/// an unreadable document is not evidence that the change was safe.
fn toml_change(base: &str, head: &str) -> ControlChange {
    let (Ok(base_table), Ok(head_table)) =
        (base.parse::<toml::Table>(), head.parse::<toml::Table>())
    else {
        return line_change(base, head);
    };
    document_change(
        serde_json::to_value(base_table),
        serde_json::to_value(head_table),
        base,
        head,
    )
}

fn yaml_change(base: &str, head: &str) -> ControlChange {
    let (Ok(base_doc), Ok(head_doc)) = (
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(base),
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(head),
    ) else {
        return line_change(base, head);
    };
    document_change(
        serde_json::to_value(base_doc),
        serde_json::to_value(head_doc),
        base,
        head,
    )
}

fn document_change(
    base_tree: serde_json::Result<serde_json::Value>,
    head_tree: serde_json::Result<serde_json::Value>,
    base: &str,
    head: &str,
) -> ControlChange {
    let (Ok(base_tree), Ok(head_tree)) = (base_tree, head_tree) else {
        return line_change(base, head);
    };
    tree_change(&base_tree, &head_tree)
}

/// Both documents as trees: every value the base had must still be there, and
/// what is new must be a new entry rather than a new attribute on an old one.
///
/// The distinction is what separates the two shapes this rule exists to tell
/// apart. `[test.lanes.broadcast]` and `lanes:broadcast:` are whole entries
/// added beside their siblings — they say nothing about the lanes already
/// judging. `allow_failure: true` inside a job that already judges is a scalar
/// landing on an existing entry, and it excuses that job from failing. Both are
/// "an added key" in tree terms; only the second changes what green means.
fn tree_change(base: &serde_json::Value, head: &serde_json::Value) -> ControlChange {
    let (Some(base_map), Some(head_map)) = (base.as_object(), head.as_object()) else {
        return if base == head {
            ControlChange::Additive
        } else {
            ControlChange::Weakening
        };
    };
    for (key, base_value) in base_map {
        let Some(head_value) = head_map.get(key) else {
            return ControlChange::Weakening;
        };
        if tree_change(base_value, head_value) == ControlChange::Weakening {
            return ControlChange::Weakening;
        }
    }
    for (key, head_value) in head_map {
        if !base_map.contains_key(key) && !head_value.is_object() {
            return ControlChange::Weakening;
        }
    }
    ControlChange::Additive
}

/// Whatever is not a document the bridge can read — Rust, shell, a justfile —
/// is additive only when every line it had survives, in order. Weakening logic
/// that already runs means editing or deleting a line it is written on.
fn line_change(base: &str, head: &str) -> ControlChange {
    let mut head_lines = head.lines();
    for line in base.lines() {
        if !head_lines.any(|candidate| candidate == line) {
            return ControlChange::Weakening;
        }
    }
    ControlChange::Additive
}

#[cfg(test)]
mod tests {
    use super::{ControlChange, classify};

    #[test]
    fn a_new_control_file_is_additive() {
        assert_eq!(
            classify(
                ".gitlab/ci/lanes.yml",
                None,
                Some("lanes:new:\n  stage: test\n")
            ),
            ControlChange::Additive
        );
    }

    #[test]
    fn a_removed_control_file_weakens() {
        assert_eq!(
            classify(
                ".gitlab/ci/lanes.yml",
                Some("lanes:old:\n  stage: test\n"),
                None
            ),
            ControlChange::Weakening
        );
    }

    /// The shape PR #184 has: a lane added beside the ones that already judge.
    #[test]
    fn a_yaml_job_added_beside_the_others_is_additive() {
        let base = "lanes:loom:\n  stage: test\n  script:\n    - just ci run linux-loom\n";
        let head = "lanes:loom:\n  stage: test\n  script:\n    - just ci run linux-loom\n\
                    lanes:broadcast:\n  allow_failure: true\n  script:\n    - just ci run linux-broadcast\n";

        assert_eq!(
            classify(".gitlab/ci/lanes.yml", Some(base), Some(head)),
            ControlChange::Additive
        );
    }

    /// The shape the rule exists to catch: a job that judged now cannot fail.
    #[test]
    fn excusing_an_existing_yaml_job_weakens() {
        let base = "lanes:loom:\n  stage: test\n";
        let head = "lanes:loom:\n  stage: test\n  allow_failure: true\n";

        assert_eq!(
            classify(".gitlab/ci/lanes.yml", Some(base), Some(head)),
            ControlChange::Weakening
        );
    }

    #[test]
    fn a_new_toml_section_is_additive() {
        let base = "[test.lanes.loom]\nfeatures = [\"loom\"]\n";
        let head = "[test.lanes.loom]\nfeatures = [\"loom\"]\n\n[test.lanes.broadcast]\nfeatures = [\"broadcast\"]\n";

        assert_eq!(
            classify(".config/xtask.toml", Some(base), Some(head)),
            ControlChange::Additive
        );
    }

    #[test]
    fn retuning_an_existing_toml_value_weakens() {
        let base = "[profile.stress]\nslow-timeout = \"120s\"\n";
        let head = "[profile.stress]\nslow-timeout = \"600s\"\n";

        assert_eq!(
            classify(".config/nextest.toml", Some(base), Some(head)),
            ControlChange::Weakening
        );
    }

    /// A lane enum gains a variant: nothing that dispatched before dispatches
    /// differently.
    #[test]
    fn appending_rust_lines_is_additive() {
        let base = "enum Lane {\n    LinuxTest,\n}\n";
        let head = "enum Lane {\n    LinuxTest,\n    LinuxBroadcast,\n}\n";

        assert_eq!(
            classify("xtask/src/ci/run.rs", Some(base), Some(head)),
            ControlChange::Additive
        );
    }

    #[test]
    fn editing_a_rust_line_weakens() {
        let base = "const REJECT_BYTES: u64 = 15_000_000_000;\n";
        let head = "const REJECT_BYTES: u64 = 1;\n";

        assert_eq!(
            classify("xtask/src/ci/run.rs", Some(base), Some(head)),
            ControlChange::Weakening
        );
    }

    #[test]
    fn an_unparsable_document_falls_back_to_the_line_rule() {
        let base = "lanes:loom:\n  script: [\n";
        let head = "lanes:loom:\n  script: [\nlanes:new:\n";

        assert_eq!(
            classify(".gitlab/ci/lanes.yml", Some(base), Some(head)),
            ControlChange::Additive
        );
    }
}
