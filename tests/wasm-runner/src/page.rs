//! The page and scripts that run one test binary in a browser.

use std::{fs, path::Path};

use anyhow::{Context, Result};

use crate::suite::{Mode, Suite};

const INDEX: &str = include_str!("page/index.html");
const BROWSER: &str = include_str!("page/browser.js");
const DEDICATED_WORKER: &str = include_str!("page/dedicated_worker.js");
const WORKER: &str = include_str!("page/worker.js");

/// How the harness reports: `nocapture` streams console output with the
/// results, otherwise it is kept aside and printed only for a failed run.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    pub(crate) nocapture: bool,
    pub(crate) include_ignored: bool,
}

/// Writes `run.js` (and `worker.js` for worker suites) next to the generated
/// bindings and returns the index page that loads them.
pub(crate) fn write(dir: &Path, suite: &Suite, options: Options, style: &str) -> Result<String> {
    let tests = serde_json::to_string(&suite.exports())?;
    let fill = |template: &str| {
        template
            .replace("__NOCAPTURE__", &options.nocapture.to_string())
            .replace("__INCLUDE_IGNORED__", &options.include_ignored.to_string())
            .replace("__FILTERED__", &suite.filtered.to_string())
            .replace("__TESTS__", &tests)
    };
    let run = match suite.mode {
        Mode::Browser => fill(BROWSER),
        Mode::DedicatedWorker => {
            fs::write(dir.join("worker.js"), fill(WORKER)).context("writing worker.js")?;
            fill(DEDICATED_WORKER)
        }
    };
    fs::write(dir.join("run.js"), run).context("writing run.js")?;
    Ok(fill(INDEX).replace("__STYLE__", style))
}
