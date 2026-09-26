use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::consts;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SystemSnapshot {
    pub(super) cgroup_v2: CgroupV2,
    pub(super) cpuset: CpuSet,
    pub(super) limits: Limits,
    pub(super) kernel: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CgroupV2 {
    pub(super) scope: CgroupScope,
    pub(super) path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CgroupScope {
    CurrentProcessCgroup,
    Unavailable,
}

impl CgroupScope {
    #[must_use]
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::CurrentProcessCgroup => "current_process_cgroup",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CpuSet {
    pub(super) cgroup_effective: Option<String>,
    pub(super) proc_allowed: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Limits {
    pub(super) cgroup_cpu_max: Option<String>,
    pub(super) cgroup_memory_max: Option<String>,
    pub(super) cgroup_pids_max: Option<String>,
    pub(super) ulimit_open_files: Option<String>,
    pub(super) ulimit_processes: Option<String>,
}

pub(super) fn capture() -> Result<SystemSnapshot> {
    capture_from(Path::new(consts::PROC_ROOT), Path::new(consts::CGROUP_ROOT))
}

fn capture_from(proc_root: &Path, cgroup_root: &Path) -> Result<SystemSnapshot> {
    let kernel_root = proc_root.join("sys/kernel");
    let kernel_type = read_required_value(&kernel_root.join("ostype"), "kernel type")?;
    let kernel_release = read_required_value(&kernel_root.join("osrelease"), "kernel release")?;
    let kernel_version = read_required_value(&kernel_root.join("version"), "kernel version")?;
    let kernel = format!(
        "{kernel_type} {kernel_release} {kernel_version} {}",
        std::env::consts::ARCH
    );

    let cgroup_source = read_required_source(&proc_root.join("self/cgroup"), "process cgroup")?;
    let resolved_cgroup = parse_current_cgroup(&cgroup_source)?
        .map(|path| resolve_cgroup_path(cgroup_root, path))
        .transpose()?;
    let cgroup_path = existing_cgroup_directory(resolved_cgroup)?;
    let scope = if cgroup_path.is_some() {
        CgroupScope::CurrentProcessCgroup
    } else {
        CgroupScope::Unavailable
    };

    let status = read_required_source(&proc_root.join("self/status"), "process status")?;
    let process_limits = read_required_source(&proc_root.join("self/limits"), "process limits")?;

    let cpuset = CpuSet {
        cgroup_effective: read_cgroup_value(cgroup_path.as_deref(), "cpuset.cpus.effective")?,
        proc_allowed: labeled_value(&status, "Cpus_allowed_list:"),
    };
    let limits = Limits {
        cgroup_cpu_max: read_cgroup_value(cgroup_path.as_deref(), "cpu.max")?,
        cgroup_memory_max: read_cgroup_value(cgroup_path.as_deref(), "memory.max")?,
        cgroup_pids_max: read_cgroup_value(cgroup_path.as_deref(), "pids.max")?,
        ulimit_open_files: limit_value(&process_limits, "Max open files"),
        ulimit_processes: limit_value(&process_limits, "Max processes"),
    };

    Ok(SystemSnapshot {
        kernel,
        cpuset,
        limits,
        cgroup_v2: CgroupV2 {
            scope,
            path: cgroup_path,
        },
    })
}

fn read_required_value(path: &Path, label: &str) -> Result<String> {
    let value = read_required_source(path, label)?;
    let value = value.trim();
    ensure!(
        !value.is_empty(),
        "{label} source is empty: {}",
        path.display()
    );
    Ok(value.to_owned())
}

fn read_required_source(path: &Path, label: &str) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("read Linux {label} source {}", path.display()))
}

fn read_cgroup_value(directory: Option<&Path>, name: &str) -> Result<Option<String>> {
    let Some(directory) = directory else {
        return Ok(None);
    };
    read_optional_value(&directory.join(name))
}

fn existing_cgroup_directory(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(Some(path)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("inspect cgroup directory {}", path.display()))
        }
    }
}

fn read_optional_value(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => {
            let value = value.trim();
            Ok((!value.is_empty()).then(|| value.to_owned()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read cgroup value {}", path.display())),
    }
}

fn parse_current_cgroup(source: &str) -> Result<Option<&str>> {
    let mut current = None;
    for line in source.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next();
        let controllers = fields.next();
        let path = fields.next();
        let (Some(hierarchy), Some(controllers), Some(path)) = (hierarchy, controllers, path)
        else {
            bail!("malformed /proc/self/cgroup entry: {line:?}");
        };
        if hierarchy == "0" && controllers.is_empty() {
            ensure!(
                current.is_none(),
                "multiple cgroup v2 entries in /proc/self/cgroup"
            );
            current = Some(path);
        }
    }
    Ok(current)
}

fn resolve_cgroup_path(root: &Path, path: &str) -> Result<PathBuf> {
    let relative = path
        .strip_prefix("/")
        .with_context(|| format!("cgroup v2 path is not absolute: {path:?}"))?;
    ensure!(
        Path::new(relative)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "cgroup v2 path contains a non-normal component: {path:?}"
    );
    Ok(root.join(relative))
}

fn labeled_value(source: &str, label: &str) -> Option<String> {
    source.lines().find_map(|line| {
        line.strip_prefix(label)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn limit_value(source: &str, label: &str) -> Option<String> {
    source.lines().find_map(|line| {
        line.strip_prefix(label)
            .and_then(|value| value.split_whitespace().next())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, value: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture directory");
        }
        fs::write(path, value).expect("write fixture");
    }

    fn required_proc_fixture(root: &Path, cgroup: &str) {
        write(&root.join("sys/kernel/ostype"), "Linux\n");
        write(&root.join("sys/kernel/osrelease"), "6.12-test\n");
        write(&root.join("sys/kernel/version"), "#1 SMP PREEMPT\n");
        write(&root.join("self/cgroup"), cgroup);
        write(
            &root.join("self/status"),
            "Name:\ttest\nCpus_allowed_list:\t2-5\n",
        );
        write(
            &root.join("self/limits"),
            "Limit                     Soft Limit           Hard Limit           Units\n\
             Max processes             4096                 4096                 processes\n\
             Max open files            1024                 2048                 files\n",
        );
    }

    #[test]
    fn captures_current_cgroup_and_keeps_missing_values_explicit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        let cgroup_root = temp.path().join("cgroup");
        required_proc_fixture(&proc_root, "0::/runner.slice/job.scope\n");
        let current = cgroup_root.join("runner.slice/job.scope");
        write(&current.join("cpuset.cpus.effective"), "2-5\n");
        write(&current.join("cpu.max"), "200000 100000\n");

        let snapshot = capture_from(&proc_root, &cgroup_root).expect("capture system");

        assert!(
            snapshot
                .kernel
                .starts_with("Linux 6.12-test #1 SMP PREEMPT")
        );
        assert_eq!(
            snapshot.cgroup_v2,
            CgroupV2 {
                scope: CgroupScope::CurrentProcessCgroup,
                path: Some(current),
            }
        );
        assert_eq!(snapshot.cpuset.cgroup_effective.as_deref(), Some("2-5"));
        assert_eq!(snapshot.cpuset.proc_allowed.as_deref(), Some("2-5"));
        assert_eq!(
            snapshot.limits.cgroup_cpu_max.as_deref(),
            Some("200000 100000")
        );
        assert_eq!(snapshot.limits.cgroup_memory_max, None);
        assert_eq!(snapshot.limits.cgroup_pids_max, None);
        assert_eq!(snapshot.limits.ulimit_open_files.as_deref(), Some("1024"));
        assert_eq!(snapshot.limits.ulimit_processes.as_deref(), Some("4096"));
    }

    #[test]
    fn records_an_unavailable_cgroup_without_root_substitution() {
        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        required_proc_fixture(&proc_root, "7:cpu:/legacy\n");

        let snapshot =
            capture_from(&proc_root, &temp.path().join("cgroup")).expect("capture system");

        assert_eq!(snapshot.cgroup_v2.scope, CgroupScope::Unavailable);
        assert_eq!(snapshot.cgroup_v2.path, None);
        assert_eq!(snapshot.cpuset.cgroup_effective, None);
        assert_eq!(snapshot.limits.cgroup_cpu_max, None);
    }

    #[test]
    fn records_a_missing_v2_directory_as_unavailable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        required_proc_fixture(&proc_root, "0::/missing.scope\n");

        let snapshot =
            capture_from(&proc_root, &temp.path().join("cgroup")).expect("capture system");

        assert_eq!(snapshot.cgroup_v2.scope, CgroupScope::Unavailable);
        assert_eq!(snapshot.cgroup_v2.path, None);
    }

    #[test]
    fn missing_kernel_source_is_an_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        required_proc_fixture(&proc_root, "0::/\n");
        fs::remove_file(proc_root.join("sys/kernel/osrelease")).expect("remove kernel source");

        let error = capture_from(&proc_root, &temp.path().join("cgroup"))
            .expect_err("missing kernel source must fail");

        assert!(error.to_string().contains("kernel release"), "{error:#}");
    }

    #[test]
    fn rejects_a_cgroup_path_that_escapes_the_v2_mount() {
        let error = resolve_cgroup_path(Path::new("/sys/fs/cgroup"), "/../elsewhere")
            .expect_err("parent traversal must fail");

        assert!(error.to_string().contains("non-normal component"));
    }
}
