use std::{fs, io::Write, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let Some(nextest_env) = std::env::var_os("NEXTEST_ENV").map(PathBuf::from) else {
        eprintln!("fixture-cache-setup: NEXTEST_ENV not set; refusing to run");
        return ExitCode::FAILURE;
    };

    let cache_dir = std::env::temp_dir().join("kithara-fixture-cache");

    // Fresh cache per run: wipe the previous run's contents, then recreate.
    let _ = fs::remove_dir_all(&cache_dir);
    if let Err(e) = fs::create_dir_all(&cache_dir) {
        eprintln!("fixture-cache-setup: create {cache_dir:?}: {e}");
        return ExitCode::FAILURE;
    }

    let line = format!("KITHARA_FIXTURE_CACHE={}\n", cache_dir.display());
    match fs::OpenOptions::new().append(true).open(&nextest_env) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                eprintln!("fixture-cache-setup: write NEXTEST_ENV: {e}");
                return ExitCode::FAILURE;
            }
        }
        Err(e) => {
            eprintln!("fixture-cache-setup: open NEXTEST_ENV: {e}");
            return ExitCode::FAILURE;
        }
    }

    println!("fixture-cache-setup: cache at {}", cache_dir.display());
    ExitCode::SUCCESS
}
