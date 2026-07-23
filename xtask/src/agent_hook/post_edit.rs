use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use super::input::HookInput;

struct Consts;

impl Consts {
    const PATCH_BYTES: usize = 16 * 1024 * 1024;
    const PATCH_OPERATIONS: usize = 4096;
    const PATH_BYTES: usize = 4096;
}

pub(super) fn run(input: &HookInput, root: &Path) -> Result<()> {
    let paths = match input.tool_name.as_str() {
        "Write" | "Edit" | "MultiEdit" => {
            vec![PathBuf::from(input.string_field("file_path")?)]
        }
        "apply_patch" => patch_paths(input.string_field("command")?)?,
        name => bail!("unsupported file-edit hook tool `{name}`"),
    };

    for path in paths {
        kithara_devtools::format::format_path(root, &path)
            .with_context(|| format!("format edited path {}", path.display()))?;
    }
    Ok(())
}

fn patch_paths(patch: &str) -> Result<Vec<PathBuf>> {
    if patch.len() > Consts::PATCH_BYTES {
        bail!(
            "apply_patch input exceeds {} byte hook limit",
            Consts::PATCH_BYTES
        );
    }

    let mut lines = patch.lines();
    if lines.next() != Some("*** Begin Patch") {
        bail!("apply_patch input must start with `*** Begin Patch`");
    }

    let mut collector = PathCollector::default();
    let mut pending_update = None;
    let mut can_move = false;
    let mut saw_operation = false;
    let mut saw_end = false;

    while let Some(line) = lines.next() {
        if line == "*** End Patch" {
            collector.record_pending(&mut pending_update);
            if lines.next().is_some() {
                bail!("apply_patch input contains content after `*** End Patch`");
            }
            saw_end = true;
            break;
        }

        if let Some(raw) = line.strip_prefix("*** Add File: ") {
            collector.record_pending(&mut pending_update);
            collector.record(raw)?;
            saw_operation = true;
            can_move = false;
            continue;
        }
        if let Some(raw) = line.strip_prefix("*** Delete File: ") {
            collector.record_pending(&mut pending_update);
            collector.validate(raw)?;
            saw_operation = true;
            can_move = false;
            continue;
        }
        if let Some(raw) = line.strip_prefix("*** Update File: ") {
            collector.record_pending(&mut pending_update);
            pending_update = Some(collector.validate(raw)?);
            saw_operation = true;
            can_move = true;
            continue;
        }
        if let Some(raw) = line.strip_prefix("*** Move to: ") {
            if !can_move || pending_update.is_none() {
                bail!("`*** Move to:` must immediately follow `*** Update File:`");
            }
            pending_update = None;
            collector.record(raw)?;
            can_move = false;
            continue;
        }
        if line.starts_with("*** ") && line != "*** End of File" {
            bail!("unrecognized apply_patch marker `{line}`");
        }
        if !saw_operation {
            bail!("apply_patch content appears before a file operation");
        }
        can_move = false;
    }

    if !saw_end {
        bail!("apply_patch input must end with `*** End Patch`");
    }
    if !saw_operation {
        bail!("apply_patch input contains no file operations");
    }
    Ok(collector.paths)
}

#[derive(Default)]
struct PathCollector {
    operations: usize,
    paths: Vec<PathBuf>,
    seen: BTreeSet<PathBuf>,
}

impl PathCollector {
    fn validate(&mut self, raw: &str) -> Result<PathBuf> {
        self.operations += 1;
        if self.operations > Consts::PATCH_OPERATIONS {
            bail!(
                "apply_patch input exceeds {} file operation hook limit",
                Consts::PATCH_OPERATIONS
            );
        }
        if raw.is_empty() || raw.len() > Consts::PATH_BYTES || raw.contains('\0') {
            bail!("invalid apply_patch path");
        }
        let path = Path::new(raw);
        if path.is_absolute() {
            bail!("apply_patch path must stay below the repository root: {raw}");
        }
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => normalized.push(value),
                Component::CurDir => {}
                _ => bail!("apply_patch path must stay below the repository root: {raw}"),
            }
        }
        if normalized.as_os_str().is_empty() {
            bail!("invalid apply_patch path");
        }
        Ok(normalized)
    }

    fn record(&mut self, raw: &str) -> Result<()> {
        let path = self.validate(raw)?;
        self.record_path(path);
        Ok(())
    }

    fn record_pending(&mut self, pending: &mut Option<PathBuf>) {
        if let Some(path) = pending.take() {
            self.record_path(path);
        }
    }

    fn record_path(&mut self, path: PathBuf) {
        if self.seen.insert(path.clone()) {
            self.paths.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::patch_paths;

    #[test]
    fn extracts_format_targets_and_ignores_deletes() {
        let paths = patch_paths(
            "*** Begin Patch\n\
             *** Add File: crates/example/src/new.rs\n\
             +fn added() {}\n\
             *** Update File: .config/example.toml\n\
             @@\n\
             -old = true\n\
             +new = true\n\
             *** Update File: old.json\n\
             *** Move to: moved.json\n\
             @@\n\
             -{}\n\
             +{\"moved\":true}\n\
             *** Delete File: deleted.rs\n\
             *** Update File: .config/example.toml\n\
             @@\n\
             -new = true\n\
             +new = false\n\
             *** End Patch",
        )
        .expect("parse patch paths");

        assert_eq!(
            paths,
            [
                PathBuf::from("crates/example/src/new.rs"),
                PathBuf::from(".config/example.toml"),
                PathBuf::from("moved.json"),
            ]
        );
    }

    #[test]
    fn rejects_patch_path_escape() {
        let error = patch_paths(
            "*** Begin Patch\n*** Update File: ../outside.rs\n@@\n-old\n+new\n*** End Patch",
        )
        .expect_err("escaping path must fail");

        assert!(format!("{error:#}").contains("repository root"));
    }

    #[test]
    fn rejects_unknown_or_incomplete_patch_grammar() {
        for patch in [
            "*** Begin Patch\n*** Copy File: source.rs\n*** End Patch",
            "*** Begin Patch\n*** Move to: moved.rs\n*** End Patch",
            "*** Begin Patch\n*** Update File: source.rs\n@@\n-old\n+new",
        ] {
            assert!(
                patch_paths(patch).is_err(),
                "patch unexpectedly parsed: {patch}"
            );
        }
    }
}
