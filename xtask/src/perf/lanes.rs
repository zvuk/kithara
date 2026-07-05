use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) flash: bool,
    pub(crate) backend: &'static str,
}

pub(crate) const LANES: [Lane; 4] = [
    Lane {
        flash: true,
        backend: "wreq",
    },
    Lane {
        flash: false,
        backend: "wreq",
    },
    Lane {
        flash: true,
        backend: "apple",
    },
    Lane {
        flash: false,
        backend: "apple",
    },
];

pub(crate) const PRIMARY_LANE: &str = "flash-on-wreq";

impl Lane {
    pub(crate) fn name(&self) -> String {
        let flash = if self.flash { "on" } else { "off" };
        format!("flash-{flash}-{}", self.backend)
    }

    pub(crate) fn by_name(name: &str) -> Option<Self> {
        LANES.into_iter().find(|lane| lane.name() == name)
    }
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

    pub(crate) fn repeat_dir(&self, lane: &str, repeat: u32) -> PathBuf {
        self.run_dir
            .join("matrix")
            .join(lane)
            .join(format!("rep{repeat}"))
    }

    pub(crate) fn matrix_dir(&self) -> PathBuf {
        self.run_dir.join("matrix")
    }

    pub(crate) fn slow_json(&self) -> PathBuf {
        self.run_dir.join("slow.json")
    }

    pub(crate) fn slow_md(&self) -> PathBuf {
        self.run_dir.join("slow.md")
    }

    pub(crate) fn profiles_dir(&self, lane: &str) -> PathBuf {
        self.run_dir.join("profiles").join(lane)
    }

    pub(crate) fn report_md(&self) -> PathBuf {
        self.run_dir.join("report.md")
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RepeatMeta {
    pub(crate) run_id: String,
    pub(crate) lane: String,
    pub(crate) features: Vec<String>,
    pub(crate) repeat: u32,
    pub(crate) commit: String,
    pub(crate) started_unix: u64,
    pub(crate) duration_secs: f64,
    pub(crate) exit_code: Option<i32>,
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

    #[test]
    fn lane_names_round_trip() {
        assert_eq!(LANES.len(), 4);
        assert_eq!(LANES[0].name(), "flash-on-wreq");
        assert_eq!(PRIMARY_LANE, "flash-on-wreq");
        for lane in LANES {
            assert_eq!(Lane::by_name(&lane.name()), Some(lane));
        }
        assert_eq!(Lane::by_name("flash-maybe-wreq"), None);
    }

    #[test]
    fn run_paths_layout() {
        let p = RunPaths::new(Path::new("/d"), "run-1");
        assert_eq!(
            p.repeat_dir("flash-on-wreq", 2),
            PathBuf::from("/d/run-1/matrix/flash-on-wreq/rep2")
        );
        assert_eq!(p.matrix_dir(), PathBuf::from("/d/run-1/matrix"));
        assert_eq!(p.slow_json(), PathBuf::from("/d/run-1/slow.json"));
        assert_eq!(p.slow_md(), PathBuf::from("/d/run-1/slow.md"));
        assert_eq!(
            p.profiles_dir("flash-on-wreq"),
            PathBuf::from("/d/run-1/profiles/flash-on-wreq")
        );
        assert_eq!(p.report_md(), PathBuf::from("/d/run-1/report.md"));
    }

    #[test]
    fn sanitize_test_names() {
        assert_eq!(sanitize("a::b::c"), "a__b__c");
        assert_eq!(sanitize("x/y z"), "x_y_z");
    }
}
