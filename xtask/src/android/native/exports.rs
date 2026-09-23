use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Result, bail};

use crate::child;

/// The entry point the instrumentation calls to hand the host transport to Rust.
const TRANSPORT_INSTALL: &str = "Java_com_kithara_net_NativeHttpTransport_install";

/// Whether the Android loader binds [`TRANSPORT_INSTALL`] for an image loaded
/// out of the staged library directory: the image or a staged library in its
/// dependency tree defines it.
pub(super) struct Binding<'a> {
    tools: &'a Path,
    libraries: &'a Path,
    cancel: Option<&'a child::Cancel>,
    defined: BTreeMap<PathBuf, bool>,
}

impl<'a> Binding<'a> {
    /// `tools` is an LLVM `bin` directory carrying `llvm-nm` and `llvm-readobj`.
    pub(super) fn new(
        tools: &'a Path,
        libraries: &'a Path,
        cancel: Option<&'a child::Cancel>,
    ) -> Self {
        Self {
            tools,
            libraries,
            cancel,
            defined: BTreeMap::new(),
        }
    }

    pub(super) fn binds(&mut self, image: &Path) -> Result<bool> {
        let mut visited = BTreeSet::from([image.to_owned()]);
        let mut pending = VecDeque::from([image.to_owned()]);
        while let Some(library) = pending.pop_front() {
            if self.defines(&library)? {
                return Ok(true);
            }
            for needed in self.needed(&library)? {
                // System libraries stay outside the staged directory.
                let needed = self.libraries.join(needed);
                if needed.is_file() && visited.insert(needed.clone()) {
                    pending.push_back(needed);
                }
            }
        }
        Ok(false)
    }

    fn defines(&mut self, library: &Path) -> Result<bool> {
        if let Some(&defined) = self.defined.get(library) {
            return Ok(defined);
        }
        let listing = self.listing(
            "llvm-nm",
            &["--dynamic", "--defined-only", "--just-symbol-name"],
            library,
        )?;
        let defined = listing.lines().any(|line| line == TRANSPORT_INSTALL);
        self.defined.insert(library.to_owned(), defined);
        Ok(defined)
    }

    fn needed(&self, image: &Path) -> Result<Vec<String>> {
        let listing = self.listing("llvm-readobj", &["--needed-libs"], image)?;
        Ok(listing
            .lines()
            .map(str::trim)
            .skip_while(|line| *line != "NeededLibraries [")
            .skip(1)
            .take_while(|line| *line != "]")
            .map(str::to_owned)
            .collect())
    }

    fn listing(&self, tool: &str, args: &[&str], file: &Path) -> Result<String> {
        let output = child::output(
            Command::new(self.tools.join(tool)).args(args).arg(file),
            self.cancel,
            Duration::from_secs(60),
        )?;
        if !output.status.success() {
            bail!(
                "{tool} {} failed: {}",
                file.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8(output.stdout)?)
    }
}

/// The fixture libraries come from NDK 27.2.12479018 clang, one C file each:
///
/// ```text
/// aarch64-linux-android24-clang -shared -nostdlib -fPIC \
///     -Wl,-soname,lib<name>.so -o lib<name>.so <name>.c [-L. -l<needed>]
/// llvm-strip --strip-unneeded lib<name>.so
/// ```
///
/// `exporter` defines the entry point, `plain` defines another symbol,
/// `dependent` calls the entry point and needs `exporter`, and `twohop` calls
/// `dependent` and needs it.
#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/android-exports")
    }

    fn tools() -> PathBuf {
        crate::sysroot::tool("llvm-nm")
            .and_then(|nm| nm.parent().map(Path::to_owned))
            .expect("the llvm-tools component carries llvm-nm")
    }

    fn binds(libraries: &Path, image: &str) -> bool {
        let tools = tools();
        Binding::new(&tools, libraries, None)
            .binds(&libraries.join(image))
            .unwrap()
    }

    #[test]
    fn an_image_that_defines_the_entry_point_binds_it() {
        assert!(binds(&fixtures(), "libexporter.so"));
    }

    #[test]
    fn an_image_without_the_entry_point_leaves_it_unbound() {
        assert!(!binds(&fixtures(), "libplain.so"));
    }

    #[test]
    fn an_image_binds_the_entry_point_of_a_staged_library_it_needs() {
        assert!(binds(&fixtures(), "libdependent.so"));
    }

    #[test]
    fn an_image_binds_the_entry_point_two_needed_libraries_away() {
        assert!(binds(&fixtures(), "libtwohop.so"));
    }

    #[test]
    fn a_needed_library_outside_the_staged_directory_binds_nothing() {
        let staged = tempfile::tempdir().unwrap();
        fs::copy(
            fixtures().join("libdependent.so"),
            staged.path().join("libdependent.so"),
        )
        .unwrap();
        assert!(!binds(staged.path(), "libdependent.so"));
    }
}
