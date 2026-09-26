use std::fmt::Write as _;

use anyhow::{Result, bail};

/// The changelog section of `version`, without its heading. A release always
/// has one: the tag step renders it into the tagged changelog.
pub(super) fn changelog_section(changelog: &str, version: &str) -> Result<String> {
    let heading = format!("## [{version}]");
    let mut lines = changelog
        .lines()
        .skip_while(|line| !line.starts_with(&heading));
    if lines.next().is_none() {
        bail!("the tagged changelog has no {heading} section");
    }
    let section = lines
        .take_while(|line| !line.starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(section.trim().to_string())
}

/// Release notes: the changelog section, where the site serves the demo and
/// the documentation, and the checksum of every attached artifact.
pub(super) fn release(body: &str, site: &str, artifacts: &[(String, String)]) -> String {
    let mut notes = format!(
        "{}\n\n## Documentation and demo\n\nThe web demo, the iOS, Android and Web API \
         documentation, and every crate on crates.io and docs.rs: {site}\n",
        body.trim()
    );
    list(&mut notes, artifacts);
    notes
}

/// Nightly notes: the commit the channel built today and what it built.
pub(super) fn nightly(
    title: &str,
    sha: &str,
    date: &str,
    subject: &str,
    artifacts: &[(String, String)],
) -> String {
    let mut notes = format!("## {title} nightly\n\nBuilt from `{sha}` ({date}).\n\n> {subject}\n");
    list(&mut notes, artifacts);
    notes.push_str(
        "\nThis release is replaced by every nightly run. Pin a version tag for anything durable.\n",
    );
    notes
}

fn list(notes: &mut String, artifacts: &[(String, String)]) {
    notes.push_str("\n## Artifacts\n\n");
    for (name, checksum) in artifacts {
        let _ = writeln!(notes, "- `{name}` — `{checksum}`");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts;

    #[test]
    fn a_release_reads_its_own_changelog_section() {
        assert_eq!(
            changelog_section(consts::CHANGELOG, "0.0.2").unwrap(),
            "### Features\n\n- **audio**: Faster seeks"
        );
        assert_eq!(
            changelog_section(consts::CHANGELOG, "0.0.1").unwrap(),
            "### Fixed\n\n- Older fix"
        );
    }

    #[test]
    fn a_release_without_a_changelog_section_stops() {
        let error = changelog_section(consts::CHANGELOG, "0.0.3").unwrap_err();

        assert!(error.to_string().contains("## [0.0.3]"), "{error}");
        assert!(changelog_section(consts::CHANGELOG, "0.0").is_err());
    }

    #[test]
    fn release_notes_carry_the_site_and_every_checksum() {
        let notes = release(
            "### Features\n\n- Faster seeks",
            "https://zvuk.github.io/kithara/",
            &[
                ("Kithara.xcframework.zip".into(), "aaa".into()),
                ("kithara.aar".into(), "bbb".into()),
            ],
        );

        assert!(
            notes.starts_with("### Features\n\n- Faster seeks\n"),
            "{notes}"
        );
        assert!(notes.contains("https://zvuk.github.io/kithara/"), "{notes}");
        assert!(
            notes.contains("- `Kithara.xcframework.zip` — `aaa`\n"),
            "{notes}"
        );
        assert!(notes.contains("- `kithara.aar` — `bbb`\n"), "{notes}");
    }
}
