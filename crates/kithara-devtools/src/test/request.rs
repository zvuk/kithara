use std::path::PathBuf;

use anyhow::{Context, Result, bail};

#[derive(Debug)]
pub(super) struct TestRequest {
    pub(super) flash: Option<bool>,
    pub(super) loom: Option<bool>,
    pub(super) net_backend: Option<String>,
    pub(super) no_block: Option<bool>,
    pub(super) load: Option<bool>,
    /// Lanes named with `--lane`; one picks the lane to run, none the
    /// default lane.
    pub(super) lanes: Vec<String>,
    /// A filterset every lane the request runs is narrowed to. A
    /// passed-through `-E` is a union member, so it could widen a selection;
    /// this one only takes tests away.
    pub(super) narrow: Option<String>,
    pub(super) expected_tests: Option<PathBuf>,
    pub(super) selection_output: Option<PathBuf>,
    pub(super) passthrough: Vec<String>,
    /// Run only the lane's share of the draft scope: the required packages and
    /// what the pull request names.
    pub(super) draft: bool,
}

impl TestRequest {
    pub(super) fn parse(args: &[String]) -> Result<Self> {
        let mut request = Self {
            lanes: Vec::new(),
            net_backend: None,
            no_block: None,
            load: None,
            loom: None,
            narrow: None,
            expected_tests: None,
            selection_output: None,
            passthrough: Vec::new(),
            flash: None,
            draft: false,
        };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--flash=off" | "--flash=false" | "--no-flash" => request.flash = Some(false),
                "--flash=on" | "--flash=true" => request.flash = Some(true),
                "--flash" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--flash requires a value"))?;
                    request.flash = Some(parse_toggle("flash", value)?);
                }
                "--no-block=off" | "--no-block=false" => request.no_block = Some(false),
                "--no-block=on" | "--no-block=true" => request.no_block = Some(true),
                "--no-block" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--no-block requires a value"))?;
                    request.no_block = Some(parse_toggle("no-block", value)?);
                }
                "--load=off" | "--load=false" => request.load = Some(false),
                "--load=on" | "--load=true" => request.load = Some(true),
                "--load" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--load requires a value"))?;
                    request.load = Some(parse_toggle("load", value)?);
                }
                "--draft" => request.draft = true,
                "--loom=off" | "--loom=false" | "--no-loom" => request.loom = Some(false),
                "--loom=on" | "--loom=true" => request.loom = Some(true),
                "--loom" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--loom requires a value"))?;
                    request.loom = Some(parse_toggle("loom", value)?);
                }
                "--lane" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--lane requires a value"))?;
                    request.lanes.push(value.clone());
                }
                "--narrow" => {
                    request.narrow = Some(filterset(iter.next().map(String::as_str))?);
                }
                "--expected-tests" => {
                    request.expect_tests(iter.next().map(String::as_str))?;
                }
                "--selection-output" => {
                    request.select_to(iter.next().map(String::as_str))?;
                }
                "--net-backend" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--net-backend requires a value"))?;
                    request.net_backend = Some(value.clone());
                }
                _ if arg.starts_with("--flash=") => {
                    let value = arg.trim_start_matches("--flash=");
                    request.flash = Some(parse_toggle("flash", value)?);
                }
                _ if arg.starts_with("--no-block=") => {
                    let value = arg.trim_start_matches("--no-block=");
                    request.no_block = Some(parse_toggle("no-block", value)?);
                }
                _ if arg.starts_with("--load=") => {
                    let value = arg.trim_start_matches("--load=");
                    request.load = Some(parse_toggle("load", value)?);
                }
                _ if arg.starts_with("--loom=") => {
                    let value = arg.trim_start_matches("--loom=");
                    request.loom = Some(parse_toggle("loom", value)?);
                }
                _ if arg.starts_with("--lane=") => {
                    let value = arg.trim_start_matches("--lane=");
                    request.lanes.push(value.to_owned());
                }
                _ if arg.starts_with("--narrow=") => {
                    request.narrow = Some(filterset(arg.strip_prefix("--narrow="))?);
                }
                _ if arg.starts_with("--expected-tests=") => {
                    request.expect_tests(arg.strip_prefix("--expected-tests="))?;
                }
                _ if arg.starts_with("--selection-output=") => {
                    request.select_to(arg.strip_prefix("--selection-output="))?;
                }
                _ if arg.starts_with("--net-backend=") => {
                    let value = arg.trim_start_matches("--net-backend=");
                    request.net_backend = Some(value.to_owned());
                }
                _ => request.passthrough.push(arg.clone()),
            }
        }
        if request.expected_tests.is_some() {
            request.validate_expected_selection()?;
        }
        Ok(request)
    }

    fn select_to(&mut self, value: Option<&str>) -> Result<()> {
        if self.selection_output.is_some() {
            bail!("--selection-output may be supplied only once");
        }
        let path = value
            .filter(|path| !path.trim().is_empty() && !path.starts_with('-'))
            .context("--selection-output needs a path")?;
        self.selection_output = Some(PathBuf::from(path));
        Ok(())
    }

    fn expect_tests(&mut self, value: Option<&str>) -> Result<()> {
        if self.expected_tests.is_some() {
            bail!("--expected-tests may be supplied only once");
        }
        match value {
            Some(path) if !path.trim().is_empty() => {
                self.expected_tests = Some(PathBuf::from(path));
                Ok(())
            }
            _ => bail!("--expected-tests needs a path to an exact identity TSV"),
        }
    }

    fn validate_expected_selection(&self) -> Result<()> {
        if self.draft {
            bail!("--expected-tests requires one lane and cannot be combined with --draft");
        }
        super::inventory::inventory_args(&self.passthrough)?;
        Ok(())
    }
}

fn filterset(value: Option<&str>) -> Result<String> {
    match value {
        Some(filterset) if !filterset.trim().is_empty() => Ok(filterset.to_owned()),
        _ => bail!("--narrow needs a filterset"),
    }
}

fn parse_toggle(name: &str, value: &str) -> Result<bool> {
    match value {
        "on" | "true" => Ok(true),
        "off" | "false" => Ok(false),
        _ => bail!("unsupported {name} mode: {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_output_is_consumed_before_forwarding_runner_arguments() {
        for args in [
            vec!["--selection-output=selection.json", "--profile", "ci"],
            vec!["--selection-output", "selection.json", "--profile", "ci"],
        ] {
            let request =
                TestRequest::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
                    .unwrap();
            assert_eq!(
                request.selection_output,
                Some(PathBuf::from("selection.json"))
            );
            assert_eq!(request.passthrough, ["--profile", "ci"]);
        }
    }

    #[test]
    fn selection_output_requires_one_unambiguous_path() {
        for args in [
            vec!["--selection-output"],
            vec!["--selection-output="],
            vec!["--selection-output", " "],
            vec!["--selection-output", "--profile", "ci"],
            vec![
                "--selection-output=one.json",
                "--selection-output",
                "two.json",
            ],
            vec![
                "--selection-output",
                "one.json",
                "--selection-output=two.json",
            ],
        ] {
            assert!(
                TestRequest::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
                    .is_err()
            );
        }
    }
}
