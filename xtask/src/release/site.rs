use std::{
    fmt::Write as _,
    fs,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::common::tools::ToolsConfig;

use super::{
    git::{Remote, committer, github_remote, output, token},
    tag::tag_of,
};
use crate::{config::ReleaseConfig, consts};

/// Where a documentation channel sits on the Pages site, as `DocC` needs it to
/// rewrite its links: the site is served under the repository name.
pub(crate) fn hosting_base(cfg: &ReleaseConfig, channel: &str) -> Result<String> {
    let (_, name) = repo_parts(&cfg.github_repo)?;
    Ok(format!("{name}/{}", cfg.docs_channel(channel)?.pages_path))
}

/// The address GitHub serves the repository's Pages site at.
pub(super) fn pages_url(repo: &str) -> Result<String> {
    let (owner, name) = repo_parts(repo)?;
    Ok(format!(
        "https://{}.github.io/{name}/",
        owner.to_ascii_lowercase()
    ))
}

fn repo_parts(repo: &str) -> Result<(&str, &str)> {
    github_remote(repo)?;
    repo.split_once('/')
        .with_context(|| format!("invalid GitHub repository name: {repo:?}"))
}

/// Lay the site out in `out`: the player at the root and every documentation
/// set under its own path, unpacked from the release artifacts, with the
/// release section `latest` added beside the player on its page.
pub(super) fn assemble(
    cfg: &ReleaseConfig,
    tools: &ToolsConfig,
    artifacts: &Path,
    latest: &str,
    out: &Path,
) -> Result<()> {
    let unpacked = out.join(".unpacked");
    let parts = [(&cfg.wasm_asset, &cfg.wasm_dist, "")].into_iter().chain(
        cfg.docs
            .values()
            .map(|docs| (&docs.asset, &docs.archive, docs.pages_path.as_str())),
    );
    for (number, (asset, source, target)) in parts.enumerate() {
        let into = unpacked.join(number.to_string());
        fs::create_dir_all(&into).with_context(|| format!("creating {}", into.display()))?;
        let status = Command::new(tools.program("unzip"))
            .arg("-q")
            .arg(artifacts.join(asset))
            .arg("-d")
            .arg(&into)
            .stdin(Stdio::null())
            .status()
            .with_context(|| format!("running unzip for {asset}"))?;
        if !status.success() {
            bail!("unzip {asset} failed ({status})");
        }
        // The release jobs zip a directory by its name, so the archive holds
        // that one directory.
        let top = Path::new(source)
            .file_name()
            .with_context(|| format!("{source} names no directory"))?;
        let from = into.join(top);
        if !from.is_dir() {
            bail!("{asset} does not hold {}/", top.to_string_lossy());
        }
        move_entries(&from, &out.join(target))?;
    }
    fs::remove_dir_all(&unpacked).with_context(|| format!("removing {}", unpacked.display()))?;
    let page = out.join("index.html");
    let player = fs::read_to_string(&page)
        .with_context(|| format!("{} holds no index.html", cfg.wasm_asset))?;
    fs::write(&page, with_latest(&player, latest)?).context("writing index.html")?;
    fs::write(out.join(".nojekyll"), "").context("writing .nojekyll")
}

/// Move what `from` holds into `to`, refusing to replace anything already
/// there.
fn move_entries(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if target.exists() {
            bail!("{} is laid out twice", target.display());
        }
        fs::rename(entry.path(), &target).with_context(|| {
            format!("moving {} to {}", entry.path().display(), target.display())
        })?;
    }
    Ok(())
}

/// The player's page with the release section beside the player in its main
/// layout and the section's style in its head.
fn with_latest(player: &str, latest: &str) -> Result<String> {
    let page = insert_before(
        player,
        "</head>",
        &format!("<style>{STYLE}</style>\n", STYLE = consts::STYLE),
    )?;
    insert_before(&page, "</main>", latest)
}

fn insert_before(page: &str, tag: &str, content: &str) -> Result<String> {
    let mut found = page.match_indices(tag);
    let (Some((at, _)), None) = (found.next(), found.next()) else {
        bail!("the player page has to close {tag} exactly once");
    };
    Ok(format!("{}{content}{}", &page[..at], &page[at..]))
}

/// Replace the Pages branch with one commit holding the site in `out`.
pub(super) fn deploy(cfg: &ReleaseConfig, out: &Path, version: &str) -> Result<()> {
    let remote = Remote::github(&cfg.github_repo, Some(&token("GH_TOKEN")?))?;
    let branch = &cfg.pages_branch;
    output(committer(out).args(["init", "-q", "-b", branch]), None)?;
    output(committer(out).args(["add", "--all"]), None)?;
    output(
        committer(out).args([
            "commit",
            "-q",
            "--no-gpg-sign",
            "-m",
            &format!("Deploy {} {version}", cfg.title),
        ]),
        None,
    )?;
    println!("[pages] pushing the site to {branch}...");
    output(
        remote.git(out)?.args([
            "push",
            "--force",
            &remote.url,
            &format!("HEAD:refs/heads/{branch}"),
        ]),
        None,
    )
    .map(drop)
}

/// The release section added to the player's page: the documentation of every
/// platform, the release archives with their checksums, and every crate on
/// crates.io and docs.rs, each behind its own tab.
pub(super) fn section(
    cfg: &ReleaseConfig,
    version: &str,
    crates: &[String],
    artifacts: &[(String, String)],
) -> String {
    let repo = format!("https://github.com/{}", cfg.github_repo);
    let tag = tag_of(version);
    let release = format!("{repo}/releases/tag/{tag}");
    let changelog = format!("{repo}/blob/{tag}/CHANGELOG.md");
    let version = escape(version);
    let mut html = String::new();

    let _ = write!(
        html,
        r#"<section id="latest" class="panel release">
<div class="panel-head"><h2>Release</h2><span class="release-version">{version}</span>
<nav class="release-nav"><a href="{release}">Notes</a><a href="{changelog}">Changelog</a><a href="{repo}">GitHub</a></nav></div>
<div class="tabs">
<input type="radio" name="release-view" id="release-docs" checked><label for="release-docs">Docs</label>
<input type="radio" name="release-view" id="release-downloads"><label for="release-downloads">Downloads</label>
<input type="radio" name="release-view" id="release-crates"><label for="release-crates">Crates<span class="release-count">{}</span></label>
<div class="tab-panel"><div class="release-docs">
"#,
        crates.len()
    );
    for docs in cfg.docs.values() {
        let _ = writeln!(
            html,
            r#"<a href="{}/{}">{}<span>API reference</span></a>"#,
            escape(&docs.pages_path),
            escape(&docs.entry),
            escape(&docs.label),
        );
    }
    html.push_str("</div></div>\n<div class=\"tab-panel\"><ul class=\"release-files\">\n");
    for (name, checksum) in artifacts {
        let _ = writeln!(
            html,
            r#"<li><a href="{repo}/releases/download/{tag}/{name}">{name}</a><code title="SHA-256 {checksum}">{checksum}</code><span>{}</span></li>"#,
            escape(&contents(cfg, name)),
            name = escape(name),
            checksum = escape(checksum),
        );
    }
    html.push_str("</ul></div>\n<div class=\"tab-panel\"><ul class=\"release-crates\">\n");
    for name in crates {
        let _ = writeln!(
            html,
            r#"<li><span>{name}</span><a href="https://crates.io/crates/{name}/{version}">crates.io</a><a href="https://docs.rs/{name}/{version}">docs.rs</a></li>"#,
            name = escape(name),
        );
    }
    let _ = write!(
        html,
        "</ul></div>\n</div>\n<p class=\"release-foot\">Built from \
         <a href=\"{repo}/tree/{tag}\">{tag}</a></p>\n</section>\n",
        tag = escape(&tag),
    );
    html
}

/// What an archive carries, by the role the release configuration gives it.
fn contents(cfg: &ReleaseConfig, name: &str) -> String {
    if name == cfg.core_asset {
        return "Swift Package binary target".into();
    }
    if name == cfg.merged_asset {
        return "XCFramework with the Swift layer".into();
    }
    if name == cfg.wasm_asset {
        return "Web demo bundle".into();
    }
    cfg.docs
        .values()
        .find(|docs| docs.asset == name)
        .map_or_else(
            || "Platform library".into(),
            |docs| format!("{} documentation", docs.label),
        )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::DocsChannel;

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            title: "Kithara".into(),
            github_repo: "zvuk/kithara".into(),
            gitlab_host: "gitlab.internal".into(),
            core_asset: "KitharaFFIInternal.xcframework.zip".into(),
            wasm_asset: "kithara-wasm-pages.zip".into(),
            docs: BTreeMap::from([(
                "apple".to_string(),
                DocsChannel {
                    asset: "Kithara-docs.zip".into(),
                    label: "iOS".into(),
                    pages_path: "docs/ios".into(),
                    entry: "documentation/".into(),
                    ..DocsChannel::default()
                },
            )]),
            ..ReleaseConfig::default()
        }
    }

    #[test]
    fn the_site_is_served_under_the_repository_name() {
        let cfg = config();

        assert_eq!(
            pages_url("Zvuk/kithara").unwrap(),
            "https://zvuk.github.io/kithara/"
        );
        assert_eq!(hosting_base(&cfg, "apple").unwrap(), "kithara/docs/ios");
        assert!(hosting_base(&cfg, "missing").is_err());
    }

    #[test]
    fn the_release_section_links_every_release_surface() {
        let section = section(
            &config(),
            "0.0.2",
            &["kithara".into(), "kithara-net".into()],
            &[("KitharaFFIInternal.xcframework.zip".into(), "abc".into())],
        );

        for link in [
            r#"href="docs/ios/documentation/""#,
            "https://github.com/zvuk/kithara/releases/tag/v0.0.2",
            "https://github.com/zvuk/kithara/releases/download/v0.0.2/KitharaFFIInternal.xcframework.zip",
            "https://crates.io/crates/kithara-net/0.0.2",
            "https://docs.rs/kithara-net/0.0.2",
        ] {
            assert!(section.contains(link), "{link} missing from the section");
        }
        assert!(section.contains("Swift Package binary target"));
        assert!(section.contains(r#"<span class="release-count">2</span>"#));
        assert!(!section.contains("gitlab.internal"));
    }

    /// The section lands beside the player in its page, every class it uses is
    /// styled by the section or by that page, every tab it opens has a panel
    /// the page shows, and it paints with the palette the page declares.
    #[test]
    fn the_release_section_joins_the_player_page() {
        let player = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/kithara-ffi/index.html"),
        )
        .expect("the player page");
        let latest = section(
            &config(),
            "0.0.2",
            &["kithara".into()],
            &[("KitharaFFIInternal.xcframework.zip".into(), "abc".into())],
        );

        let page = with_latest(&player, &latest).unwrap();
        let style = page.find("<style>\n.release{").expect("section style");
        let at = page.find(r#"<section id="latest""#).expect("section");
        assert!(style < page.find("</head>").unwrap());
        assert!(page.find(r#"id="playlist""#).unwrap() < at);
        assert!(at < page.find("</main>").unwrap());

        let styled = |class: &str| {
            [consts::STYLE, player.as_str()].into_iter().any(|css| {
                css.match_indices(&format!(".{class}")).any(|(at, found)| {
                    !css[at + found.len()..]
                        .starts_with(|next: char| next.is_ascii_alphanumeric() || next == '-')
                })
            })
        };
        for class in latest
            .split(r#"class=""#)
            .skip(1)
            .filter_map(|rest| rest.split_once('"'))
            .flat_map(|(names, _)| names.split_whitespace())
        {
            assert!(styled(class), "nothing styles .{class}");
        }

        let tabs = latest.matches(r#"type="radio""#).count();
        assert_eq!(latest.matches(r#"class="tab-panel""#).count(), tabs);
        for tab in 1..=tabs {
            assert!(
                player.contains(&format!(".tab-panel:nth-of-type({tab})")),
                "the player page never shows tab panel {tab}"
            );
        }

        for name in consts::STYLE
            .split("var(--")
            .skip(1)
            .filter_map(|rest| rest.split_once(')'))
            .map(|(name, _)| name)
        {
            assert!(
                player.contains(&format!("--{name}:")),
                "the player page declares no --{name}"
            );
        }
        assert!(with_latest("<html><head></head><body></body></html>", "").is_err());
    }

    #[test]
    fn page_text_is_escaped() {
        assert_eq!(
            escape(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
