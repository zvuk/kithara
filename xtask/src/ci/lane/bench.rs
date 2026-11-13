use std::process::Command;

use anyhow::Result;

use crate::ci::{config::CiPins, process::Process};

pub(super) fn prepare(
    process: &Process,
    pins: &CiPins,
    cargo: &str,
    arguments: &[String],
    command: &mut Command,
) -> Result<()> {
    if !arguments
        .iter()
        .map(String::as_str)
        .eq(["perf", "bench", "ci"])
        || process.environment_var("RUN_BENCHMARKS").as_deref() != Some("1".as_ref())
        || process
            .environment_var("BENCH_BASE_REF")
            .is_none_or(|value| value.is_empty())
    {
        return Ok(());
    }
    if let Some(bin) = super::tools::prepare(process, pins, cargo, super::tools::Tool::Critcmp)? {
        super::tools::prepend(command, &bin)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use super::*;
    use crate::ci::config::fixture;

    #[test]
    fn head_only_and_disabled_benchmarks_need_no_comparison_tool() {
        for (run, base) in [("0", "main"), ("1", "")] {
            let process = Process::new(
                Path::new("/checkout"),
                BTreeMap::from([
                    ("RUN_BENCHMARKS".into(), run.into()),
                    ("BENCH_BASE_REF".into(), base.into()),
                ]),
            );
            let mut command = process.command("just");
            prepare(
                &process,
                &fixture().pins,
                "missing-cargo",
                &["perf".into(), "bench".into(), "ci".into()],
                &mut command,
            )
            .unwrap();
        }
    }
}
