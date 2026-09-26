use crate::consts;

/// Where the running `xtask` came from, for the job's cache summary.
///
/// The bootstrap is the most expensive cache miss a job can have - it is a
/// cargo build of this binary, ahead of the lane's own work - and until now a
/// log gave no way to tell a job that paid it from one that did not.
pub(crate) fn provenance() -> (&'static str, String) {
    let Ok(binary) = std::env::current_exe() else {
        return ("unknown", "this executable has no path".to_owned());
    };
    let shown = binary.display().to_string();
    let cached = binary.ancestors().any(|step| {
        step.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == consts::CACHE_DIRECTORY)
    });
    if cached {
        ("reused", format!("ran the cached generation at {shown}"))
    } else {
        ("miss", format!("built from source for this job at {shown}"))
    }
}
