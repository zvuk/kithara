use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, bail};

use super::device_features;
use crate::BuildProfile;

type Features = BTreeMap<String, BTreeSet<String>>;

fn product(root: &Path, target: &str) -> Result<Features> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "tree",
            "-p",
            "kithara-ffi",
            "--no-default-features",
            "--features",
            device_features(BuildProfile::Release),
            "--target",
            target,
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}|{f}",
        ])
        .output()
        .context("resolving the Android artifact composition")?;
    if !output.status.success() {
        bail!(
            "Android artifact resolution failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    parse(&String::from_utf8(output.stdout)?)
}

fn parse(tree: &str) -> Result<Features> {
    let mut packages = Features::new();
    for line in tree.lines().filter(|line| !line.is_empty()) {
        let (package, features) = line
            .split_once('|')
            .context("Cargo tree omitted the resolved feature list")?;
        let name = package
            .split_whitespace()
            .next()
            .context("Cargo tree omitted the package name")?;
        let features = features.strip_suffix(" (*)").unwrap_or(features).trim();
        packages.entry(name.to_owned()).or_default().extend(
            features
                .split(',')
                .filter(|f| !f.is_empty())
                .map(str::to_owned),
        );
    }
    Ok(packages)
}

pub(super) fn verify(
    root: &Path,
    target: &str,
    inventory: &Command,
    evidence: &Path,
) -> Result<()> {
    if !inventory
        .get_args()
        .any(|arg| arg == "--no-default-features")
    {
        bail!("Android test inventory must disable package defaults");
    }
    let expected = product(root, target)?;
    let mut tree = Command::new("cargo");
    tree.current_dir(root).args([
        "tree",
        "--edges",
        "normal,dev",
        "--prefix",
        "none",
        "--format",
        "{p}|{f}",
    ]);
    let mut args = inventory.get_args();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--workspace" | "--no-default-features" | "--all-features") => {
                tree.arg(arg);
            }
            Some("--features" | "--exclude" | "--target" | "-p" | "--package") => {
                tree.arg(arg)
                    .arg(args.next().context("missing Cargo selection value")?);
            }
            _ => {}
        }
    }
    let output = tree
        .output()
        .context("resolving the Android test composition")?;
    if !output.status.success() {
        bail!(
            "Android test resolution failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let actual = parse(&String::from_utf8(output.stdout)?)?;
    std::fs::write(
        evidence.join("product-composition.json"),
        serde_json::to_vec_pretty(&expected)?,
    )?;
    std::fs::write(
        evidence.join("test-composition.json"),
        serde_json::to_vec_pretty(&actual)?,
    )?;
    compare(&expected, &actual)
}

fn compare(expected: &Features, actual: &Features) -> Result<()> {
    for package in actual.keys() {
        if (package.starts_with("symphonia")
            || package.starts_with("fdk-aac")
            || package.starts_with("ffmpeg")
            || package == "kithara-mpa")
            && !expected.contains_key(package)
        {
            bail!(
                "Android composition mismatch: backend dependency {package} is absent from the artifact"
            );
        }
    }

    for (package, features) in actual
        .iter()
        .filter(|(name, _)| name.starts_with("kithara"))
    {
        if package.ends_with("-tests")
            || package.starts_with("kithara-test-")
            || package == "kithara-core-test-fixtures"
        {
            continue;
        }
        let product = expected.get(package);
        if product.is_none() && package != "kithara-encode" {
            bail!(
                "Android composition mismatch: {package} is in tests but absent from the artifact"
            );
        }
        let empty = BTreeSet::new();
        let product = product.unwrap_or(&empty);
        let expected_features = relevant(package, product, expected);
        let actual_features = relevant(package, features, expected);
        if expected_features != actual_features {
            bail!(
                "Android composition mismatch for {package}: artifact={expected_features:?}, tests={actual_features:?}"
            );
        }
    }
    for package in expected.keys().filter(|name| name.starts_with("kithara")) {
        if !actual.contains_key(package) {
            bail!(
                "Android composition mismatch: {package} is in the artifact but absent from tests"
            );
        }
    }
    Ok(())
}

fn relevant<'a>(
    package: &str,
    features: &'a BTreeSet<String>,
    product: &Features,
) -> BTreeSet<&'a str> {
    features
        .iter()
        .map(String::as_str)
        .filter(|feature| {
            if matches!(
                (package, *feature),
                ("kithara-ffi", "dev" | "test")
                    | ("kithara" | "kithara-host", "offline")
                    | ("kithara-platform", "signal")
            ) {
                return false;
            }
            if matches!(
                *feature,
                "default"
                    | "mock"
                    | "probe"
                    | "flash"
                    | "hang"
                    | "tokio-net"
                    | "tokio-rt-multi-thread"
            ) {
                return false;
            }
            if package != "kithara-net"
                && (feature.starts_with("client-") || feature.starts_with("tls-"))
            {
                return false;
            }
            !(package == "kithara" && product.contains_key(&format!("kithara-{feature}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_entry_points_are_checked_against_release_backends() {
        let product =
            parse("kithara-ffi v1|android,uniffi\nkithara-decode v1|android\nkithara-host v1|\n")
                .unwrap();
        let debug = parse("kithara-ffi v1|android,dev,test,uniffi\nkithara-decode v1|android,probe\nkithara-host v1|offline\n").unwrap();
        compare(&product, &debug).unwrap();
        let mut leaked = debug;
        leaked
            .get_mut("kithara-decode")
            .unwrap()
            .insert("symphonia".into());
        assert!(compare(&product, &leaked).is_err());
    }

    #[test]
    fn test_support_cannot_enable_a_software_decoder() {
        let product = parse("kithara-decode v1|android\nkithara-net v1|client-wreq\n").unwrap();
        let leaked =
            parse("kithara-decode v1|android,symphonia\nkithara-net v1|client-wreq\n").unwrap();
        assert!(compare(&product, &leaked).is_err());
        let direct =
            parse("kithara-decode v1|android\nkithara-net v1|client-wreq\nsymphonia v1|\n")
                .unwrap();
        assert!(compare(&product, &direct).is_err());
        let instrumented = parse("kithara-decode v1|android,mock,probe\nkithara-net v1|client-wreq,mock\nkithara-test-dylib v1|\n").unwrap();
        compare(&product, &instrumented).unwrap();
    }

    #[test]
    fn product_mpeg_parser_does_not_authorize_a_software_codec() {
        let product =
            parse("kithara-decode v1|android\nkithara-mpa v1|\nsymphonia-core v1|\n").unwrap();
        compare(&product, &product).unwrap();
        let mut leaked = product.clone();
        leaked.insert("symphonia-bundle-mp3".into(), Default::default());
        assert!(compare(&product, &leaked).is_err());
    }

    #[test]
    fn test_helpers_cannot_add_a_second_network_backend_or_app() {
        let product = parse("kithara-net v1|client-wreq\n").unwrap();
        for tree in [
            "kithara-net v1|client-wreq,client-reqwest\n",
            "kithara-net v1|client-wreq\nkithara-app v1|\n",
        ] {
            assert!(compare(&product, &parse(tree).unwrap()).is_err());
        }
    }

    #[test]
    fn fixture_types_do_not_allow_an_encoder_backend() {
        let product = Features::new();
        compare(&product, &parse("kithara-encode v1|\n").unwrap()).unwrap();
        assert!(compare(&product, &parse("kithara-encode v1|ffmpeg\n").unwrap()).is_err());
    }
}
