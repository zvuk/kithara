use std::{fs, path::PathBuf, process::Command};

/// The tool `name` a component installed into the active Rust sysroot, such
/// as `llvm-nm` from `llvm-tools`.
pub(crate) fn tool(name: &str) -> Option<PathBuf> {
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sysroot = String::from_utf8(output.stdout).ok()?;
    let rustlib = PathBuf::from(sysroot.trim()).join("lib/rustlib");
    for entry in fs::read_dir(rustlib).ok()? {
        let path = entry.ok()?.path().join("bin").join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}
