use url::{Position, Url};

use super::{
    AssetLayout, AssetResource, AssetSource, local_root, remote_root,
    validation::{
        append_component_suffix, encode_component, encode_url_component, encode_url_leaf,
        fingerprint,
    },
};

struct Consts;
impl Consts {
    const DEFAULT_EXTENSION: &'static str = "bin";
    const MAX_EXTENSION_LEN: usize = 16;
}

/// Default portable cache layout shared by file, HLS, and named artifacts.
#[derive(Debug, Default)]
pub struct DefaultLayout;

impl AssetLayout for DefaultLayout {
    fn root(&self, source: &AssetSource) -> String {
        match source {
            AssetSource::Remote { url, discriminator } => {
                remote_root(url, discriminator.as_deref())
            }
            AssetSource::Local { path } => local_root(path),
        }
    }

    fn path(&self, resource: &AssetResource) -> String {
        match resource {
            AssetResource::Source { extension } => source_path(extension),
            AssetResource::Url(url) => url_mirror(url),
            AssetResource::Named { namespace, name } => {
                format!("{}/{}", named_namespace(namespace), encode_component(name))
            }
        }
    }
}

fn named_namespace(namespace: &str) -> String {
    if namespace == "track" {
        "~ntrack".to_string()
    } else {
        encode_component(namespace)
    }
}

fn authority_component(url: &Url) -> String {
    let authority = &url[Position::BeforeHost..Position::AfterPort];
    let encoded = encode_url_component(authority);
    if url.scheme() == "https" && url.username().is_empty() && url.password().is_none() {
        return encoded;
    }

    let origin = &url[..Position::BeforePath];
    let suffix = format!("~o{}", fingerprint(origin.as_bytes()));
    append_component_suffix(&encoded, &suffix, origin.as_bytes())
}

fn source_path(extension: &str) -> String {
    let extension = if !extension.is_empty()
        && extension.len() <= Consts::MAX_EXTENSION_LEN
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !extension.eq_ignore_ascii_case("tmp")
    {
        extension.to_ascii_lowercase()
    } else {
        Consts::DEFAULT_EXTENSION.to_string()
    };
    let leaf = encode_component(&format!("track.{extension}"));
    format!("track/{leaf}")
}

fn url_mirror(url: &Url) -> String {
    if url.host().is_none() || url.path_segments().is_none() {
        return String::new();
    }

    let path = url.path().strip_prefix('/').unwrap_or_else(|| url.path());
    let mut segments = path.split('/').peekable();
    let mut parts = vec!["track".to_string(), authority_component(url)];
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            parts.push(encode_url_leaf(segment, url.query()));
        } else {
            parts.push(encode_url_component(segment));
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn url(value: &str) -> Url {
        Url::parse(value).expect("valid test URL")
    }

    #[kithara::test]
    fn default_remote_root_preserves_legacy_identity() {
        let source = AssetSource::Remote {
            url: url("https://stream.silvercomet.top/hls/master.m3u8?token=secret#player"),
            discriminator: None,
        };

        assert_eq!(
            DefaultLayout.root(&source),
            "3d74111283a80b5cc0b463b3376d9fed"
        );
    }

    #[kithara::test]
    #[case("mp3", "track/track.mp3")]
    #[case("MP3", "track/track.mp3")]
    #[case("", "track/track.bin")]
    #[case(".mp3", "track/track.bin")]
    #[case("m/4a", "track/track.bin")]
    #[case("abcdefghijklmnopq", "track/track.bin")]
    #[case("tmp", "track/track.bin")]
    fn default_source_path(#[case] extension: &str, #[case] expected: &str) {
        let resource = AssetResource::Source {
            extension: extension.to_string(),
        };

        assert_eq!(DefaultLayout.path(&resource), expected);
    }

    #[kithara::test]
    fn default_silvercomet_url_mirrors_server_tree() {
        let resource = AssetResource::Url(url("https://stream.silvercomet.top/hls/master.m3u8"));

        assert_eq!(
            DefaultLayout.path(&resource),
            "track/stream.silvercomet.top/hls/master.m3u8"
        );
    }

    #[kithara::test]
    fn default_query_identity_is_ordered_and_fragment_free() {
        let first = AssetResource::Url(url("https://cdn.example.com/a/seg.m4s?a=1&b=2#first"));
        let second = AssetResource::Url(url("https://cdn.example.com/a/seg.m4s?b=2&a=1#second"));

        assert_eq!(
            DefaultLayout.path(&first),
            "track/cdn.example.com/a/seg~q8e85be58c1c372ac29fe7bfa80d8ddcb.m4s"
        );
        assert_eq!(
            DefaultLayout.path(&second),
            "track/cdn.example.com/a/seg~qa746b90cddac3e075db2f0c7b65aa5d0.m4s"
        );
    }

    #[kithara::test]
    fn default_url_preserves_ports_percent_encoding_and_empty_segments() {
        let resource = AssetResource::Url(url("https://cdn.example.com:8443/a%2Fb//seg%20one.m4s"));

        assert_eq!(
            DefaultLayout.path(&resource),
            "track/cdn.example.com~3a8443/a~p2fb/~e/seg~p20one.m4s"
        );
    }

    #[kithara::test]
    fn default_url_case_is_safe_on_case_insensitive_filesystems() {
        let upper = AssetResource::Url(url("https://cdn.example.com/A/file.M4S"));
        let lower = AssetResource::Url(url("https://cdn.example.com/a/file.m4s"));
        let upper = DefaultLayout.path(&upper);
        let lower = DefaultLayout.path(&lower);

        assert_eq!(upper, "track/cdn.example.com/~41/file.~4d4~53");
        assert_eq!(lower, "track/cdn.example.com/a/file.m4s");
        assert_ne!(upper.to_ascii_lowercase(), lower.to_ascii_lowercase());
    }

    #[kithara::test]
    fn default_transport_identity_does_not_leak_credentials() {
        let http = AssetResource::Url(url("http://cdn.example.com/a.m4s"));
        let authenticated = AssetResource::Url(url("https://user:pass@cdn.example.com/a.m4s"));
        let authenticated_path = DefaultLayout.path(&authenticated);

        assert_eq!(
            DefaultLayout.path(&http),
            "track/cdn.example.com~oaa6457e6b94853c0d3a761279c11cfb9/a.m4s"
        );
        assert_eq!(
            authenticated_path,
            "track/cdn.example.com~o57c2e979b06c055e78a37c0a2f182533/a.m4s"
        );
        assert!(!authenticated_path.contains("user"));
        assert!(!authenticated_path.contains("pass"));
    }

    #[kithara::test]
    fn default_named_path_uses_semantic_components() {
        let analysis = AssetResource::Named {
            namespace: "analysis".to_string(),
            name: "track.analysis".to_string(),
        };
        let escaped = AssetResource::Named {
            namespace: "Wave/Forms".to_string(),
            name: "Track".to_string(),
        };

        assert_eq!(DefaultLayout.path(&analysis), "analysis/track.analysis");
        assert_eq!(DefaultLayout.path(&escaped), "~57ave~2f~46orms/~54rack");
    }

    #[kithara::test]
    fn default_named_path_cannot_alias_source_path() {
        let source = AssetResource::Source {
            extension: "mp3".to_string(),
        };
        let named = AssetResource::Named {
            namespace: "track".to_string(),
            name: "track.mp3".to_string(),
        };

        assert_eq!(DefaultLayout.path(&named), "~ntrack/track.mp3");
        assert_ne!(DefaultLayout.path(&source), DefaultLayout.path(&named));
    }

    #[kithara::test]
    #[cfg(unix)]
    fn default_local_root_is_stable_for_unix_paths() {
        let source = AssetSource::Local {
            path: "/tmp/track.mp3".into(),
        };

        assert_eq!(
            DefaultLayout.root(&source),
            "43ca3fc0b9c7bd2d511c5b6cb6e1c83f"
        );
    }

    #[kithara::test]
    #[cfg(windows)]
    fn default_local_root_is_stable_for_windows_paths() {
        let source = AssetSource::Local {
            path: r"C:\Music\track.mp3".into(),
        };

        assert_eq!(
            DefaultLayout.root(&source),
            "02b191765d653bea58c333b52595da4f"
        );
    }
}
