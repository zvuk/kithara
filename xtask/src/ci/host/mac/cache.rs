use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};

use super::services;
use crate::ci::config::CiConfig;

pub(super) fn launchd(
    config: &CiConfig,
    binary_directory: &Path,
    settings: &str,
) -> Result<String> {
    let root = config.host.host_root.join("services/ci-cache");
    let home = config
        .host
        .host_root
        .join("home")
        .join(&config.host.ci_user);
    let mut environment = budgets(settings)?;
    environment.extend([
        ("HOME".into(), root.join("admin-home").display().to_string()),
        (
            "XDG_CONFIG_HOME".into(),
            root.join("admin-home/.config").display().to_string(),
        ),
        ("RUSTFS_ADDRESS".into(), "127.0.0.1:9000".into()),
        ("RUSTFS_CONSOLE_ENABLE".into(), "false".into()),
        ("CACHE_CLIENT_UID".into(), config.host.ci_uid.to_string()),
        (
            "CACHE_CLIENT_ENDPOINT".into(),
            "http://127.0.0.1:9000".into(),
        ),
    ]);
    let path = std::env::join_paths([
        binary_directory.to_path_buf(),
        config.host.brew_root.join("bin"),
        home.join(".cargo/bin"),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ])?
    .into_string()
    .map_err(|_| anyhow::anyhow!("native cache PATH is not UTF-8"))?;
    let mut arguments = vec!["/usr/bin/env".to_owned()];
    arguments.extend(
        environment
            .into_iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    arguments.extend([
        config
            .host
            .host_root
            .join("toolchains/shared-bin/kithara-ci")
            .display()
            .to_string(),
        "ci".into(),
        "cache".into(),
        "serve".into(),
        "--root".into(),
        root.display().to_string(),
    ]);
    Ok(services::launchd(
        "com.zvuk.kithara-ci.cache",
        &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
        &config.host.host_root.join("logs/cache.log"),
        &path,
        "<key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>",
    ))
}

fn budgets(settings: &str) -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for line in settings
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (key, value) = line
            .split_once('=')
            .context("invalid cache environment entry")?;
        if key != "CACHE_SCOPES"
            && key != "CACHE_BUCKET_QUOTA"
            && !key.starts_with("CACHE_BUCKET_QUOTA_")
        {
            continue;
        }
        let value = value.trim();
        let value = if value.starts_with('"') {
            value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .context("unterminated cache environment value")?
        } else {
            value
        };
        ensure!(
            !value.is_empty() && !value.chars().any(char::is_control),
            "invalid cache budget value"
        );
        ensure!(
            environment
                .insert(key.to_owned(), value.to_owned())
                .is_none(),
            "duplicate cache budget"
        );
    }
    for key in ["CACHE_SCOPES", "CACHE_BUCKET_QUOTA"] {
        ensure!(environment.contains_key(key), "native cache requires {key}");
    }
    Ok(environment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_service_retains_scopes_budgets_and_private_identity() {
        let config = crate::ci::config::fixture();
        let definition = launchd(&config, Path::new("/ci/rustfs/1.0.1"),
            "CACHE_SCOPES=\"trusted review quarantine\"\nCACHE_BUCKET_QUOTA=20GiB\nCACHE_BUCKET_QUOTA_REVIEW=40GiB\nCACHE_CLIENT_ENDPOINT=http://old:19000\n").unwrap();
        let plist = plist::Value::from_reader_xml(definition.as_bytes()).unwrap();
        let dictionary = plist.as_dictionary().unwrap();
        let args = dictionary["ProgramArguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_string().unwrap())
            .collect::<Vec<_>>();
        for expected in [
            "CACHE_SCOPES=trusted review quarantine",
            "CACHE_BUCKET_QUOTA_REVIEW=40GiB",
            "RUSTFS_ADDRESS=127.0.0.1:9000",
            "RUSTFS_CONSOLE_ENABLE=false",
            "CACHE_CLIENT_ENDPOINT=http://127.0.0.1:9000",
        ] {
            assert!(args.contains(&expected), "{args:?}");
        }
        assert!(args.iter().any(|arg| arg.ends_with("/ci-cache/admin-home")));
        assert!(!definition.contains("old:19000"));
        assert!(!definition.contains("docker"));
        assert_eq!(dictionary["KeepAlive"].as_boolean(), Some(true));
        assert!(
            dictionary["EnvironmentVariables"].as_dictionary().unwrap()["PATH"]
                .as_string()
                .unwrap()
                .starts_with("/ci/rustfs/1.0.1:")
        );
    }

    #[test]
    fn missing_duplicate_or_unterminated_budgets_fail_installation() {
        for settings in [
            "",
            "CACHE_SCOPES=review",
            "CACHE_SCOPES=review\nCACHE_SCOPES=trusted\nCACHE_BUCKET_QUOTA=20GiB",
            "CACHE_SCOPES=\"review\nCACHE_BUCKET_QUOTA=20GiB",
        ] {
            assert!(budgets(settings).is_err(), "{settings}");
        }
    }
}
