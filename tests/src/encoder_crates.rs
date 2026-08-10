use std::collections::BTreeSet;

use toml::{Table, Value};

/// The crates whose resolved build determines an encoded byte. Read from the
/// lockfile by name, so a version bump to any of them still lands in a fresh
/// namespace.
pub(crate) const ENCODERS: &[&str] = &["fdk-aac", "fdk-aac-sys", "ffmpeg-next", "ffmpeg-sys-next"];

// Keeping byte-neutral crates separate makes every addition an explicit
// cache-invalidation decision.
pub(crate) const NON_ENCODING_DEPENDENCIES: &[&str] =
    &["bon", "num-traits", "tempfile", "thiserror", "tracing"];

pub(crate) struct Lockfile {
    packages: Vec<Value>,
}

impl Lockfile {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut lock: Table = text
            .parse()
            .map_err(|_| "lockfile must be TOML".to_owned())?;
        let Some(Value::Array(packages)) = lock.remove("package") else {
            return Err("lockfile must list packages".to_owned());
        };

        Ok(Self { packages })
    }

    /// Return what the fingerprint hashes for [`ENCODERS`], rather than the
    /// lockfile.
    ///
    /// The whole file was hashed here before, on the reasoning that a dependency
    /// version determines the encoded bytes. Only these ones do: bumping an XML
    /// parser cannot change an AAC frame, but hashing the file said it could, so
    /// every unrelated dependency bump moved the namespace and left the suite with
    /// an empty cache — and a cold cache means per-test ffmpeg re-encodes, which
    /// blow the budgets of tests that assert against a deadline. That is the same
    /// failure the `src/native` narrowing above was written to stop; the lockfile
    /// was the remaining wide input.
    pub(crate) fn encoder_versions(&self) -> Result<Vec<String>, String> {
        let mut resolved: Vec<String> = self
            .packages
            .iter()
            .filter_map(|package| {
                let name = package.get("name").and_then(Value::as_str)?;
                ENCODERS.contains(&name).then(|| {
                    let field = |key| package.get(key).and_then(Value::as_str).unwrap_or_default();
                    // The checksum pins the exact bytes the version resolves to,
                    // which a re-release under one version number would not.
                    format!("{name} {} {}", field("version"), field("checksum"))
                })
            })
            .collect();
        resolved.sort();

        // A crate leaving the lock under a name listed here would silently stop
        // being tracked, and an untracked encoder change is a cache serving bytes
        // it should not. Fail the build instead.
        if resolved.len() != ENCODERS.len() {
            return Err(format!(
                "resolved {resolved:?} for {ENCODERS:?}; update ENCODERS to match the crates that \
                 encode fixtures"
            ));
        }

        Ok(resolved)
    }

    pub(crate) fn unclassified_encode_dependencies(&self) -> Result<BTreeSet<String>, String> {
        let encoder = self
            .packages
            .iter()
            .find(|package| {
                package.get("name").and_then(Value::as_str) == Some("kithara-encode")
                    && package.get("source").is_none()
            })
            .ok_or_else(|| "lockfile must list the workspace kithara-encode package".to_owned())?;
        let dependencies = encoder
            .get("dependencies")
            .and_then(Value::as_array)
            .ok_or_else(|| "kithara-encode must list dependencies".to_owned())?;

        dependencies
            .iter()
            .try_fold(BTreeSet::new(), |mut unclassified, dependency| {
                let dependency = dependency
                    .as_str()
                    .ok_or_else(|| "package dependency must be a string".to_owned())?;
                let package = self
                    .package_for_dependency(dependency)
                    .ok_or_else(|| "direct dependency must resolve to a package".to_owned())?;
                let name = package
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "package must have a name".to_owned())?;

                if package.get("source").is_some()
                    && !ENCODERS.contains(&name)
                    && !NON_ENCODING_DEPENDENCIES.contains(&name)
                {
                    unclassified.insert(name.to_owned());
                }

                Ok(unclassified)
            })
    }

    fn package_for_dependency(&self, dependency: &str) -> Option<&Value> {
        let mut fields = dependency.split_whitespace();
        let name = fields.next()?;
        let version = fields.next();
        let source = fields
            .next()
            .map(|source| source.trim_matches(&['(', ')'][..]));

        self.packages.iter().find(|package| {
            package.get("name").and_then(Value::as_str) == Some(name)
                && version.is_none_or(|version| {
                    package.get("version").and_then(Value::as_str) == Some(version)
                })
                && source.is_none_or(|source| {
                    package.get("source").and_then(Value::as_str) == Some(source)
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BTreeSet, Lockfile};

    const UNCLASSIFIED_DIRECT_DEPENDENCY: &str = r#"
version = 4

[[package]]
name = "kithara-encode"
version = "0.0.1"
dependencies = ["mp3lame-sys 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)"]

[[package]]
name = "mp3lame-sys"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    const CLASSIFIED_ENCODER_DEPENDENCY: &str = r#"
version = 4

[[package]]
name = "kithara-encode"
version = "0.0.1"
dependencies = ["fdk-aac 0.7.0"]

[[package]]
name = "fdk-aac"
version = "0.7.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    const TRANSITIVE_DEPENDENCY_OF_LOCAL_PACKAGE: &str = r#"
version = 4

[[package]]
name = "kithara-encode"
version = "0.0.1"
dependencies = ["kithara-workspace-hack"]

[[package]]
name = "kithara-workspace-hack"
version = "0.0.0"
dependencies = ["mp3lame-sys"]

[[package]]
name = "mp3lame-sys"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    const MISSING_ENCODER: &str = r#"
version = 4

[[package]]
name = "fdk-aac"
version = "0.8.0"
checksum = "fdk-aac-checksum"

[[package]]
name = "fdk-aac-sys"
version = "0.5.0"
checksum = "fdk-aac-sys-checksum"

[[package]]
name = "ffmpeg-next"
version = "8.1.0"
checksum = "ffmpeg-next-checksum"
"#;

    fn unaccounted(lockfile: &str) -> BTreeSet<String> {
        Lockfile::parse(lockfile)
            .and_then(|lockfile| lockfile.unclassified_encode_dependencies())
            .expect("test lockfile must describe kithara-encode dependencies")
    }

    #[test]
    fn encoder_versions_rejects_missing_encoder() {
        let lockfile = Lockfile::parse(MISSING_ENCODER).expect("test lockfile must be TOML");

        assert!(lockfile.encoder_versions().is_err());
    }

    #[test]
    fn unclassified_direct_external_dependency_is_reported() {
        let unaccounted = unaccounted(UNCLASSIFIED_DIRECT_DEPENDENCY);

        assert_eq!(unaccounted, BTreeSet::from(["mp3lame-sys".to_owned()]));
    }

    #[test]
    fn classified_encoder_dependency_is_not_reported() {
        let unaccounted = unaccounted(CLASSIFIED_ENCODER_DEPENDENCY);

        assert!(unaccounted.is_empty());
    }

    #[test]
    fn dependency_through_local_package_is_not_reported() {
        let unaccounted = unaccounted(TRANSITIVE_DEPENDENCY_OF_LOCAL_PACKAGE);

        assert!(unaccounted.is_empty());
    }
}
