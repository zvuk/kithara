use std::{
    fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use kithara_test_fixtures::assets::by_name;
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_fixtures::store;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(name), Some(out), None) = (args.next(), args.next().map(PathBuf::from), args.next())
    else {
        eprintln!("usage: kithara-fixture-export <accessor-name|--manifest> <output-path>");
        return ExitCode::FAILURE;
    };
    let name = name.to_string_lossy();
    if let Err(error) = export(&name, &out) {
        eprintln!("kithara-fixture-export: {error}");
        return ExitCode::FAILURE;
    }
    println!("kithara-fixture-export: {name} -> {}", out.display());
    ExitCode::SUCCESS
}

fn export(name: &str, out: &Path) -> io::Result<()> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    if name == "--manifest" {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let manifest = store::manifest()?;
            let bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
            fs::write(out, bytes)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixture manifests require a native filesystem",
            ))
        }
    } else {
        let asset = by_name(name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no generated asset is named `{name}`"),
            )
        })?;
        fs::write(out, asset.try_bytes().map_err(io::Error::other)?)
    }
}
