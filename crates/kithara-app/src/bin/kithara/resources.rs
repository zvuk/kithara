use std::path::{Path, PathBuf};

pub(super) fn directory(executable: &Path) -> Option<PathBuf> {
    let parent = executable.parent()?;
    if parent.file_name().is_some_and(|name| name == "MacOS")
        && parent
            .parent()?
            .file_name()
            .is_some_and(|name| name == "Contents")
    {
        return Some(parent.parent()?.join("Resources"));
    }
    Some(parent.to_path_buf())
}

pub(super) fn configuration(executable: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let resources = directory(executable)?;
    if resources != executable.parent()? {
        return Some(home?.join("Library/Application Support/Kithara/kithara.yaml"));
    }
    Some(resources.join("kithara.yaml"))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn relocated_bundle_uses_its_own_resources() {
        assert_eq!(
            directory(Path::new("/Some Folder/Kithara.app/Contents/MacOS/kithara")),
            Some(PathBuf::from("/Some Folder/Kithara.app/Contents/Resources"))
        );
    }

    #[kithara::test(native, flash(false))]
    fn unpackaged_binary_uses_its_directory() {
        assert_eq!(
            directory(Path::new("/checkout/target/release/kithara")),
            Some(PathBuf::from("/checkout/target/release"))
        );
        assert_eq!(
            directory(Path::new("/unrelated/MacOS/kithara")),
            Some(PathBuf::from("/unrelated/MacOS"))
        );
    }

    #[kithara::test(native, flash(false))]
    fn bundle_preferences_never_modify_signed_resources() {
        let executable = Path::new("/Applications/Kithara.app/Contents/MacOS/kithara");
        assert_eq!(
            configuration(executable, Some(Path::new("/Users/colleague"))),
            Some(PathBuf::from(
                "/Users/colleague/Library/Application Support/Kithara/kithara.yaml"
            ))
        );
        assert_eq!(configuration(executable, None), None);
        assert_eq!(
            configuration(Path::new("/checkout/target/release/kithara"), None),
            Some(PathBuf::from("/checkout/target/release/kithara.yaml"))
        );
    }
}
