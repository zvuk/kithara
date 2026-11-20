//! The tests one Wasm binary exports and the JS bindings that load them.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use wasm_bindgen_cli_support::Bindgen;

/// The module name the generated bindings and the page agree on.
const MODULE: &str = "wasm-bindgen-test";
/// `#[wasm_bindgen_test]` exports each test under this prefix.
const TEST_PREFIX: &str = "__wbgt_";
/// `wasm_bindgen_test_configure!` records the requested mode here.
const MODE_SECTION: &str = "__wasm_bindgen_test_unstable";

/// Where the tests run. Only the two modes this workspace configures exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    Browser,
    DedicatedWorker,
}

impl Mode {
    /// Without a configured mode the binary runs on the page, the mode every
    /// browser lane selects. Any other recorded mode is refused.
    fn from_section(data: Option<&[u8]>) -> Result<Self> {
        match data {
            None => Ok(Self::Browser),
            Some(bytes) if bytes.contains(&0x01) => Ok(Self::Browser),
            Some(bytes) if bytes.contains(&0x02) => Ok(Self::DedicatedWorker),
            Some(bytes) => bail!("unsupported wasm-bindgen-test mode {bytes:?}"),
        }
    }
}

/// Which tests to run, as cargo and the libtest-style flags name them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Filter {
    pub(crate) pattern: Option<String>,
    pub(crate) exact: bool,
}

impl Filter {
    fn admits(&self, name: &str) -> bool {
        match &self.pattern {
            None => true,
            Some(pattern) if self.exact => name == pattern,
            Some(pattern) => name.contains(pattern.as_str()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Test {
    pub(crate) name: String,
    export: String,
}

pub(crate) struct Suite {
    pub(crate) mode: Mode,
    pub(crate) tests: Vec<Test>,
    pub(crate) filtered: usize,
    module: walrus::Module,
}

impl Suite {
    pub(crate) fn read(path: &Path, filter: &Filter) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let mut module = walrus::ModuleConfig::new()
            .generate_dwarf(true)
            .parse(&bytes)
            .context("parsing the Wasm module")?;
        let (tests, filtered) = select(module.exports.iter().map(|e| e.name.as_str()), filter);
        let section = module.customs.remove_raw(MODE_SECTION);
        let mode = Mode::from_section(section.as_ref().map(|s| s.data.as_slice()))?;
        Ok(Self {
            mode,
            tests,
            filtered,
            module,
        })
    }

    pub(crate) fn exports(&self) -> Vec<&str> {
        self.tests.iter().map(|test| test.export.as_str()).collect()
    }

    /// Generates the ES-module bindings into `dir`.
    pub(crate) fn generate(&mut self, dir: &Path) -> Result<()> {
        let module = std::mem::take(&mut self.module);
        Bindgen::new()
            .web(true)?
            .debug(true)
            .input_module(MODULE, module)
            .emit_start(false)
            .generate(dir)
            .context("generating wasm-bindgen glue")
    }
}

/// Test exports look like `__wbgt_<modifiers>_<n>::<path>`; `$` among the
/// modifiers marks an ignored test, which the harness itself reports.
fn select<'a>(exports: impl Iterator<Item = &'a str>, filter: &Filter) -> (Vec<Test>, usize) {
    let mut tests = Vec::new();
    let mut filtered = 0;
    for export in exports {
        if !export.starts_with(TEST_PREFIX) {
            continue;
        }
        let Some((_, name)) = export.split_once("::") else {
            continue;
        };
        if filter.admits(name) {
            tests.push(Test {
                name: name.to_owned(),
                export: export.to_owned(),
            });
        } else {
            filtered += 1;
        }
    }
    (tests, filtered)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const EXPORTS: [&str; 5] = [
        "__wbgt_passes_0::browser::passes",
        "__wbgt_$_ignored_1::browser::ignored",
        "__wbgt_passes_2::browser::passes_too",
        "__wbindgen_malloc",
        "__wbgb_bench_0::browser::bench",
    ];

    fn names(filter: &Filter) -> (Vec<String>, usize) {
        let (tests, filtered) = select(EXPORTS.into_iter(), filter);
        (tests.into_iter().map(|test| test.name).collect(), filtered)
    }

    #[kithara::test(native)]
    fn selects_only_test_exports() {
        let (names, filtered) = names(&Filter::default());
        assert_eq!(
            names,
            ["browser::passes", "browser::ignored", "browser::passes_too"]
        );
        assert_eq!(filtered, 0);
    }

    #[kithara::test(native)]
    fn substring_and_exact_filters_count_what_they_drop() {
        let substring = Filter {
            pattern: Some("passes".into()),
            exact: false,
        };
        assert_eq!(
            names(&substring),
            (
                vec!["browser::passes".into(), "browser::passes_too".into()],
                1
            )
        );
        let exact = Filter {
            pattern: Some("browser::passes".into()),
            exact: true,
        };
        assert_eq!(names(&exact), (vec!["browser::passes".into()], 2));
    }

    #[kithara::test(native)]
    fn mode_section_selects_browser_or_dedicated_worker() {
        assert_eq!(Mode::from_section(None).unwrap(), Mode::Browser);
        assert_eq!(Mode::from_section(Some(&[0x01])).unwrap(), Mode::Browser);
        assert_eq!(
            Mode::from_section(Some(&[0x02, 0x02])).unwrap(),
            Mode::DedicatedWorker
        );
        assert_eq!(
            Mode::from_section(Some(&[0x02, 0x01])).unwrap(),
            Mode::Browser
        );
        for unsupported in [0x03, 0x04, 0x05, 0x06] {
            assert!(Mode::from_section(Some(&[unsupported])).is_err());
        }
    }
}
