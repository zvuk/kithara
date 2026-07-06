use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::common::project::{PerfConfig, PerfLane};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) backend: String,
    pub(crate) flash: bool,
}

impl Lane {
    pub(crate) fn by_name(config: &PerfConfig, name: &str) -> Option<Self> {
        Self::configured(config)
            .into_iter()
            .find(|lane| lane.name() == name)
    }

    pub(crate) fn configured(config: &PerfConfig) -> Vec<Self> {
        config.lanes.iter().map(Self::from).collect()
    }

    pub(crate) fn name(&self) -> String {
        let flash = if self.flash { "on" } else { "off" };
        format!("flash-{flash}-{}", self.backend)
    }
}

impl From<&PerfLane> for Lane {
    fn from(lane: &PerfLane) -> Self {
        Self {
            flash: lane.flash,
            backend: lane.backend.clone(),
        }
    }
}

pub(crate) fn primary_lane_name(config: &PerfConfig) -> Result<String> {
    if !config.primary_lane.is_empty() {
        return Ok(config.primary_lane.clone());
    }
    let Some(lane) = config.lanes.first() else {
        bail!("perf.primary_lane is empty and perf.lanes has no entries");
    };
    Ok(Lane::from(lane).name())
}

pub(crate) struct RunPaths {
    pub(crate) run_dir: PathBuf,
}

impl RunPaths {
    pub(crate) fn new(data_dir: &Path, run_id: &str) -> Self {
        Self {
            run_dir: data_dir.join(run_id),
        }
    }

    pub(crate) fn matrix_dir(&self) -> PathBuf {
        self.run_dir.join("matrix")
    }

    pub(crate) fn profiles_dir(&self, lane: &str) -> PathBuf {
        self.run_dir.join("profiles").join(lane)
    }

    pub(crate) fn repeat_dir(&self, lane: &str, repeat: u32) -> PathBuf {
        self.run_dir
            .join("matrix")
            .join(lane)
            .join(format!("rep{repeat}"))
    }

    pub(crate) fn report_md(&self) -> PathBuf {
        self.run_dir.join("report.md")
    }

    pub(crate) fn slow_json(&self) -> PathBuf {
        self.run_dir.join("slow.json")
    }

    pub(crate) fn slow_md(&self) -> PathBuf {
        self.run_dir.join("slow.md")
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RepeatMeta {
    pub(crate) exit_code: Option<i32>,
    pub(crate) commit: String,
    pub(crate) lane: String,
    pub(crate) run_id: String,
    pub(crate) features: Vec<String>,
    pub(crate) duration_secs: f64,
    pub(crate) repeat: u32,
    pub(crate) started_unix: u64,
}

pub(crate) fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn config() -> PerfConfig {
        PerfConfig {
            lanes: vec![
                PerfLane {
                    flash: true,
                    backend: "http".to_owned(),
                },
                PerfLane {
                    flash: false,
                    backend: "http".to_owned(),
                },
            ],
            primary_lane: String::new(),
            frame_prefix: None,
            nextest_profile: "perf".to_owned(),
        }
    }

    #[test]
    fn lane_names_round_trip() {
        let config = config();
        let lanes = Lane::configured(&config);

        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].name(), "flash-on-http");
        assert_eq!(
            primary_lane_name(&config).expect("primary lane"),
            "flash-on-http"
        );
        for lane in lanes {
            assert_eq!(Lane::by_name(&config, &lane.name()), Some(lane));
        }
        assert_eq!(Lane::by_name(&config, "flash-maybe-http"), None);
    }

    #[test]
    fn run_paths_layout() {
        let p = RunPaths::new(Path::new("/d"), "run-1");
        assert_eq!(
            p.repeat_dir("flash-on-http", 2),
            PathBuf::from("/d/run-1/matrix/flash-on-http/rep2")
        );
        assert_eq!(p.matrix_dir(), PathBuf::from("/d/run-1/matrix"));
        assert_eq!(p.slow_json(), PathBuf::from("/d/run-1/slow.json"));
        assert_eq!(p.slow_md(), PathBuf::from("/d/run-1/slow.md"));
        assert_eq!(
            p.profiles_dir("flash-on-http"),
            PathBuf::from("/d/run-1/profiles/flash-on-http")
        );
        assert_eq!(p.report_md(), PathBuf::from("/d/run-1/report.md"));
    }

    #[test]
    fn sanitize_test_names() {
        assert_eq!(sanitize("a::b::c"), "a__b__c");
        assert_eq!(sanitize("x/y z"), "x_y_z");
    }
}
