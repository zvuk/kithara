use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;
use tracing::info;

use super::required;
use crate::ci::host::mac::{read_secret, write_secure};

fn secret(path: &Path) -> Result<String> {
    let mut bytes = [0; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let value = hex::encode(bytes);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(value.as_bytes())?;
            Ok(value)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => read_secret(path),
        Err(error) => Err(error).context("create cache credential"),
    }
}

pub(super) fn credentials() -> Result<()> {
    let root = Path::new("/config");
    fs::create_dir_all(root)?;
    if !root.join("admin-user").exists() {
        write_secure(&root.join("admin-user"), "kithara-cache-admin")?;
    }
    secret(&root.join("admin-password"))?;
    Ok(())
}

fn mc(arguments: &[&str]) -> Result<()> {
    let status = Command::new("mc")
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("start cache administration client")?;
    // Arguments and client diagnostics can include credentials.
    ensure!(
        status.success(),
        "cache administration operation failed: {status}"
    );
    Ok(())
}

/// The disk a scope's bucket is allowed, which is not one number for the
/// fleet.
///
/// The scopes hold different things: the trusted one carries the layers every
/// job restores, a review scope carries whatever the branches under it happen
/// to publish. They were sized apart on the live host by hand, and a single
/// `CACHE_BUCKET_QUOTA` meant the next initialize would flatten them back to
/// one value - measured as 200 and 800 gibibytes standing against an
/// environment that still said 50. A scope may name its own, and the shared
/// value is what a scope that does not is given.
fn scope_quota(scope: &str, shared: &str) -> String {
    let named = format!(
        "CACHE_BUCKET_QUOTA_{}",
        scope.to_ascii_uppercase().replace('-', "_")
    );
    env::var(&named).unwrap_or_else(|_| shared.to_owned())
}

fn scope_bucket(scope: &str) -> Result<String> {
    ensure!(
        !scope.is_empty()
            && scope.len() <= 48
            && scope.ends_with(|character: char| character.is_ascii_alphanumeric())
            && scope
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "cache scope must contain 1..48 lowercase letters, digits or hyphens and end in a letter or digit"
    );
    Ok(format!("kithara-{scope}"))
}

pub(super) fn initialize() -> Result<()> {
    let scopes = required("CACHE_SCOPES")?;
    let quota = required("CACHE_BUCKET_QUOTA")?;
    let endpoint = required("CACHE_CLIENT_ENDPOINT")?;
    let uid = required("CACHE_CLIENT_UID")?.parse::<u32>()?;
    let url = reqwest::Url::parse(&endpoint)?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && !endpoint.chars().any(char::is_whitespace),
        "cache endpoint must be an HTTP URL without whitespace"
    );
    for scope in scopes.split_whitespace() {
        scope_bucket(scope)?;
    }
    let root = Path::new("/config");
    mc(&[
        "alias",
        "set",
        "--",
        "ci",
        "http://cache:9000",
        &read_secret(&root.join("admin-user"))?,
        &read_secret(&root.join("admin-password"))?,
    ])?;
    for scope in scopes.split_whitespace() {
        initialize_scope(scope, &scope_quota(scope, &quota), &endpoint, uid)?;
    }
    Ok(())
}

fn initialize_scope(scope: &str, quota: &str, endpoint: &str, uid: u32) -> Result<()> {
    let bucket = scope_bucket(scope)?;
    let destination = format!("ci/{bucket}");
    let directory = Path::new("/clients").join(scope);
    fs::create_dir_all(&directory)?;
    let key = secret(&directory.join("access-key"))?;
    let password = secret(&directory.join("secret-key"))?;
    mc(&["mb", "--ignore-existing", &destination])?;
    mc(&["quota", "set", &destination, "--size", quota])?;
    let mut lifecycle = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(&mut lifecycle, &retention())?;
    let status = Command::new("mc")
        .args(["ilm", "rule", "import", &destination])
        .stdin(File::open(lifecycle.path())?)
        .stdout(Stdio::null())
        .status()?;
    ensure!(status.success(), "cache lifecycle import failed: {status}");
    mc(&["admin", "user", "add", "ci", &key, &password])?;
    let mut policy_file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(&mut policy_file, &policy(scope, &bucket))?;
    mc(&[
        "admin",
        "policy",
        "create",
        "ci",
        &bucket,
        policy_file
            .path()
            .to_str()
            .context("cache policy path must be UTF-8")?,
    ])?;
    mc(&["admin", "policy", "attach", "ci", &bucket, "--user", &key])?;
    write_environment(&directory, &bucket, endpoint, &key, &password)?;
    let status = Command::new("chown")
        .args(["-R", &uid.to_string()])
        .arg(&directory)
        .status()?;
    ensure!(status.success(), "cache client ownership failed: {status}");
    info!(%scope, "compiler cache scope initialized");
    Ok(())
}

/// Where sccache keeps its objects inside a scope's bucket.
///
/// They used to sit at the bucket root with no common prefix, which is why
/// retention had to be a single unfiltered rule expiring everything after a
/// day. That rule also governed the snapshot layers, so a multi-gigabyte
/// source layer would have been republished daily. Naming the compiler cache
/// makes retention expressible per layer. The cost is paid once: existing
/// compiler-cache objects sit at the old keys and are not read again.
pub(crate) const SCCACHE_PREFIX: &str = "sccache";

/// How long each layer in a scope's bucket lives.
///
/// The compiler cache keeps its day: it is large, churns with every commit,
/// and a miss costs one compilation. The snapshot layers are keyed by content
/// (a target fingerprint, a `Cargo.lock`), so an object still named by a lock
/// file is still the right answer weeks later, and expiring it daily would
/// mean paying the full fetch every morning to rebuild the same bytes.
/// `MinIO` applies the earliest matching expiry, so these prefixes must not
/// overlap.
fn retention() -> serde_json::Value {
    json!({
        "Rules": [
            {
                "ID": "compiler-cache", "Status": "Enabled",
                "Filter": {"Prefix": format!("{SCCACHE_PREFIX}/")},
                "Expiration": {"Days": 1}
            },
            {
                "ID": "target-snapshots", "Status": "Enabled",
                "Filter": {"Prefix": "target-snapshots/"},
                "Expiration": {"Days": 7}
            },
            {
                "ID": "source-snapshots", "Status": "Enabled",
                "Filter": {"Prefix": "source-snapshots/"},
                "Expiration": {"Days": 30}
            }
        ]
    })
}

fn policy(scope: &str, bucket: &str) -> serde_json::Value {
    let mut statements = vec![
        json!({
            "Effect": "Allow",
            "Action": ["s3:ListBucket", "s3:GetBucketLocation"],
            "Resource": [format!("arn:aws:s3:::{bucket}")]
        }),
        json!({
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:PutObject"],
            "Resource": [format!("arn:aws:s3:::{bucket}/*")]
        }),
    ];
    if scope != "trusted" {
        let trusted = "kithara-trusted";
        statements.extend([
            json!({
                "Effect": "Allow",
                "Action": ["s3:ListBucket"],
                "Resource": [format!("arn:aws:s3:::{trusted}")],
                "Condition": {"StringLike": {"s3:prefix": ["target-snapshots/*", "source-snapshots/*"]}}
            }),
            json!({
                "Effect": "Allow",
                "Action": ["s3:GetObject"],
                "Resource": [
                    format!("arn:aws:s3:::{trusted}/target-snapshots/*"),
                    format!("arn:aws:s3:::{trusted}/source-snapshots/*")
                ]
            }),
        ]);
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

fn write_environment(
    directory: &Path,
    bucket: &str,
    endpoint: &str,
    key: &str,
    password: &str,
) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    for (name, value) in [
        ("SCCACHE_BUCKET", bucket),
        ("SCCACHE_S3_KEY_PREFIX", SCCACHE_PREFIX),
        ("SCCACHE_ENDPOINT", endpoint),
        ("SCCACHE_REGION", "us-east-1"),
        (
            "SCCACHE_S3_USE_SSL",
            if endpoint.starts_with("https://") {
                "true"
            } else {
                "false"
            },
        ),
        ("AWS_ACCESS_KEY_ID", key),
        ("AWS_SECRET_ACCESS_KEY", password),
        ("AWS_EC2_METADATA_DISABLED", "true"),
    ] {
        writeln!(file, "{name}={value}")?;
    }
    file.persist(directory.join("cache.env"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retention rule expires only what sits under the compiler-cache
    /// prefix, so a runner whose environment drops the prefix writes to the
    /// bucket root, where nothing expires, until the quota refuses every write.
    #[test]
    fn a_provisioned_environment_reaches_the_client_with_its_key_prefix() {
        let directory = tempfile::tempdir().unwrap();
        write_environment(directory.path(), "bucket", "http://cache", "key", "secret").unwrap();

        let environment = super::super::client_environment(&directory.path().join("cache.env"))
            .expect("the client reads what provisioning writes");

        assert_eq!(
            environment.get("SCCACHE_S3_KEY_PREFIX").map(String::as_str),
            Some(SCCACHE_PREFIX)
        );
    }

    /// Scopes were sized apart on the live host and a shared quota would flatten
    /// them on the next initialize, so a scope names its own and only a scope
    /// that says nothing takes the shared one.
    #[test]
    fn a_scope_keeps_the_quota_it_names() {
        // SAFETY: nextest runs each test in its own process.
        unsafe {
            env::set_var("CACHE_BUCKET_QUOTA_REVIEW", "800GiB");
        }

        assert_eq!(scope_quota("review", "50GiB"), "800GiB");
        assert_eq!(scope_quota("trusted", "50GiB"), "50GiB");
    }

    #[test]
    fn cache_scope_cannot_escape_its_bucket() {
        for scope in [
            "",
            "../trusted",
            "review/trusted",
            "UPPER",
            "review-",
            "a
b",
        ] {
            assert!(scope_bucket(scope).is_err(), "{scope:?}");
        }
        assert_eq!(scope_bucket("review-1").unwrap(), "kithara-review-1");
    }

    #[test]
    fn untrusted_scopes_can_only_read_trusted_target_snapshots() {
        let review = policy("review", "kithara-review");
        let statements = review["Statement"].as_array().unwrap();
        let trusted = statements
            .iter()
            .map(serde_json::Value::to_string)
            .find(|statement| {
                statement.contains("kithara-trusted/target-snapshots/*")
                    && statement.contains("s3:GetObject")
            })
            .unwrap();
        assert!(trusted.contains("target-snapshots/*"));
        assert!(trusted.contains("s3:GetObject"));
        assert!(!trusted.contains("s3:PutObject"));

        let trusted = policy("trusted", "kithara-trusted").to_string();
        assert!(!trusted.contains("target-snapshots/*"));
    }

    /// The source layer is published by the default branch and read by every
    /// branch. Without this grant a review job asks the trusted bucket for the
    /// layer, is refused, and fetches every dependency from the internet.
    #[test]
    fn untrusted_scopes_read_but_never_write_the_trusted_source_layer() {
        let review = policy("review", "kithara-review").to_string();
        assert!(review.contains("kithara-trusted/source-snapshots/*"));
        assert!(!review.contains(r#"["s3:PutObject"],"Resource":["arn:aws:s3:::kithara-trusted"#));

        let trusted = policy("trusted", "kithara-trusted").to_string();
        assert!(!trusted.contains("source-snapshots/*"));
    }

    #[test]
    fn credential_initialization_preserves_existing_values() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("password");
        let original = secret(&path).unwrap();
        assert_eq!(secret(&path).unwrap(), original);
        assert_eq!(original.len(), 64);
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    /// `MinIO` applies the earliest matching expiry, so an unfiltered rule would
    /// silently govern the snapshot prefixes too - which is what expired a
    /// content-keyed source layer after a day and would have made a
    /// multi-gigabyte object a daily republish.
    #[test]
    fn each_layer_carries_its_own_retention_and_no_rule_is_unfiltered() {
        let rules = retention();
        let rules = rules["Rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 3);

        let mut days = std::collections::BTreeMap::new();
        for rule in rules {
            let prefix = rule["Filter"]["Prefix"].as_str().expect("prefix");
            assert!(!prefix.is_empty(), "an unfiltered rule governs every layer");
            assert!(prefix.ends_with('/'), "{prefix} must name a whole prefix");
            days.insert(
                prefix.to_owned(),
                rule["Expiration"]["Days"].as_u64().expect("days"),
            );
        }

        let compiler = days[&format!("{SCCACHE_PREFIX}/")];
        assert!(
            days["source-snapshots/"] > compiler && days["target-snapshots/"] > compiler,
            "a content-keyed snapshot must outlive the compiler cache: {days:?}"
        );
    }
}
