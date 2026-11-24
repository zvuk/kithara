use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{Seek as _, Write as _},
    net::Ipv4Addr,
    process::Stdio,
};

use anyhow::{Context, Result};
use sha2::{Digest as _, Sha256};
use tracing::info;

use super::profile::{LinuxHost, parse_subnet};
use crate::{ci::process::Process, consts};

/// Replace only this subnet's owned policy in one filter-table transaction.
pub(super) fn apply(process: &Process, host: &LinuxHost) -> Result<()> {
    let mut existing = BTreeMap::new();
    for hook in ["DOCKER-USER", "INPUT"] {
        existing.insert(
            hook,
            process.capture("iptables", &["-S", hook], "read subnet fence hook")?,
        );
    }
    let text = transaction(host, &existing)?;
    let mut input = tempfile::tempfile().context("stage subnet fence transaction")?;
    input.write_all(text.as_bytes())?;
    input.rewind()?;
    let mut command = process.command("iptables-restore");
    command
        .args(["--noflush", "--wait", "5"])
        .stdin(Stdio::from(input));
    process.run_command(&mut command, "replace owned subnet fence")?;
    info!(subnet = host.subnet, "runner subnet fenced");
    Ok(())
}

fn chain(host: &LinuxHost, hook: &str) -> String {
    let digest = hex::encode(Sha256::digest(format!(
        "{}\0{}\0{hook}",
        host.network, host.subnet
    )));
    format!("KITHARA-{}", &digest[..16])
}

fn rules(host: &LinuxHost) -> Vec<Vec<String>> {
    let mut policy = Vec::new();
    for hook in ["DOCKER-USER", "INPUT"] {
        let name = chain(host, hook);
        policy.push(vec![
            "-A".into(),
            name.clone(),
            "-m".into(),
            "conntrack".into(),
            "--ctstate".into(),
            "ESTABLISHED,RELATED".into(),
            "-j".into(),
            "RETURN".into(),
        ]);
        for destination in &host.private_egress {
            policy.push(vec![
                "-A".into(),
                name.clone(),
                "-d".into(),
                destination.ip().to_string(),
                "-p".into(),
                "tcp".into(),
                "--dport".into(),
                destination.port().to_string(),
                "-j".into(),
                "RETURN".into(),
            ]);
        }
        if hook == "DOCKER-USER" {
            for destination in consts::PRIVATE_BLOCKS {
                policy.push(vec![
                    "-A".into(),
                    name.clone(),
                    "-d".into(),
                    destination.into(),
                    "-j".into(),
                    "DROP".into(),
                ]);
            }
        }
        policy.push(vec![
            "-A".into(),
            name,
            "-j".into(),
            if hook == "INPUT" { "DROP" } else { "RETURN" }.into(),
        ]);
    }
    policy
}

fn transaction(host: &LinuxHost, existing: &BTreeMap<&str, String>) -> Result<String> {
    let (address, prefix) = parse_subnet(&host.subnet)?;
    let source = format!(
        "{}/{}",
        Ipv4Addr::from(u32::from(address) & (u32::MAX << (32 - prefix))),
        prefix
    );
    let mut text = String::from("*filter\n");
    for hook in ["DOCKER-USER", "INPUT"] {
        let name = chain(host, hook);
        writeln!(text, ":{name} - [0:0]\n-F {name}")?;
    }
    for rule in rules(host) {
        writeln!(text, "{}", rule.join(" "))?;
    }
    for hook in ["DOCKER-USER", "INPUT"] {
        let name = chain(host, hook);
        let listing = existing
            .get(hook)
            .context("missing native fence hook observation")?;
        let jump = format!("-A {hook} -s {source} -j {name}");
        for _ in listing.lines().filter(|line| *line == jump) {
            writeln!(text, "-D {hook} -s {source} -j {name}")?;
        }
        writeln!(text, "-I {hook} 1 -s {source} -j {name}")?;
    }
    text.push_str("COMMIT\n");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{host::linux::profile::tests::gitlab_fixture, process::Recording};

    fn first_verdict<'a>(
        policy: &'a [Vec<String>],
        host: &LinuxHost,
        hook: &str,
        destination: std::net::SocketAddrV4,
        established: bool,
    ) -> &'a str {
        let own = chain(host, hook);
        for rule in policy.iter().filter(|rule| rule.get(1) == Some(&own)) {
            let option = |name: &str| {
                rule.windows(2)
                    .find(|pair| pair[0] == name)
                    .map(|pair| pair[1].as_str())
            };
            if option("--ctstate").is_some() && !established {
                continue;
            }
            if let Some(address) = option("-d") {
                let (network, prefix) = address
                    .split_once('/')
                    .map_or((address, 32), |(network, prefix)| {
                        (network, prefix.parse::<u32>().unwrap())
                    });
                let mask = u32::MAX << (32 - prefix);
                if u32::from(*destination.ip()) & mask
                    != u32::from(network.parse::<Ipv4Addr>().unwrap()) & mask
                {
                    continue;
                }
            }
            if let Some(port) = option("--dport")
                && port.parse::<u16>().unwrap() != destination.port()
            {
                continue;
            }
            return rule.last().unwrap();
        }
        "RETURN"
    }

    #[test]
    fn declared_and_established_flows_cannot_override_foreign_same_flow_or_global_denies() {
        let mut host = gitlab_fixture();
        host.subnet = "172.30.0.0/24".to_owned();
        let destination = "10.20.30.40:443".parse().unwrap();
        host.private_egress = vec![destination];
        let policy = rules(&host);
        for hook in ["DOCKER-USER", "INPUT"] {
            for foreign in [
                format!("-A {hook} -s 172.30.0.0/24 -d 10.20.30.40 -p tcp --dport 443 -j DROP"),
                format!("-A {hook} -j DROP"),
            ] {
                let mut hooks =
                    BTreeMap::from([("DOCKER-USER", String::new()), ("INPUT", String::new())]);
                hooks.insert(hook, foreign.clone());
                let text = transaction(&host, &hooks).unwrap();
                assert!(!text.contains(&foreign.replacen("-A ", "-D ", 1)));
                for established in [false, true] {
                    let owned = first_verdict(&policy, &host, hook, destination, established);
                    let denied = if owned == "RETURN" { "DROP" } else { owned };
                    assert_eq!(denied, "DROP", "owned policy hid {foreign}");
                    let permitted = if owned == "RETURN" { "ACCEPT" } else { owned };
                    assert_eq!(permitted, "ACCEPT");
                }
            }
            assert_eq!(
                first_verdict(
                    &policy,
                    &host,
                    hook,
                    "10.20.30.41:443".parse().unwrap(),
                    false
                ),
                "DROP"
            );
        }
    }

    #[test]
    fn native_control_endpoint_is_explicitly_allowed_without_opening_its_private_network() {
        let mut host = gitlab_fixture();
        host.private_egress = vec!["10.20.30.40:443".parse().unwrap()];
        let policy = rules(&host);
        assert!(policy.iter().any(
            |rule| rule.windows(2).any(|pair| pair == ["-d", "10.20.30.40"])
                && rule.windows(2).any(|pair| pair == ["--dport", "443"])
                && rule.last().is_some_and(|action| action == "RETURN")
        ));
        assert!(
            policy
                .iter()
                .filter(|rule| rule.last().is_some_and(|action| action == "RETURN")
                    && rule.iter().any(|arg| arg == "-d"))
                .all(|rule| rule.windows(2).any(|pair| pair == ["-d", "10.20.30.40"]))
        );
        assert!(
            policy
                .iter()
                .any(|rule| rule.get(1) == Some(&chain(&host, "INPUT"))
                    && rule.last().is_some_and(|action| action == "DROP"))
        );
    }

    #[test]
    fn empty_exceptions_preserve_established_flows_and_block_new_private_connections() {
        let host = super::super::profile::tests::host_fixture();
        assert!(host.private_egress.is_empty());
        let policy = rules(&host);
        for hook in ["DOCKER-USER", "INPUT"] {
            let name = chain(&host, hook);
            let scoped = policy
                .iter()
                .filter(|rule| rule.get(1) == Some(&name))
                .collect::<Vec<_>>();
            assert_eq!(
                scoped[0],
                &vec![
                    "-A".to_owned(),
                    name,
                    "-m".to_owned(),
                    "conntrack".to_owned(),
                    "--ctstate".to_owned(),
                    "ESTABLISHED,RELATED".to_owned(),
                    "-j".to_owned(),
                    "RETURN".to_owned()
                ]
            );
            assert!(
                scoped
                    .iter()
                    .skip(1)
                    .all(|rule| rule.last().is_none_or(|action| action != "ACCEPT"))
            );
        }
    }

    #[test]
    fn changed_policy_replaces_only_owned_chains_and_keeps_hooks_scoped() {
        let mut host = gitlab_fixture();
        host.private_egress = vec!["10.20.30.40:443".parse().unwrap()];
        let jump = format!(
            "-A DOCKER-USER -s {} -j {}",
            host.subnet,
            chain(&host, "DOCKER-USER")
        );
        let foreign = "-A DOCKER-USER -s 192.168.0.0/16 -j FOREIGN";
        let hooks = BTreeMap::from([
            ("DOCKER-USER", format!("{jump}\n{foreign}\n{jump}\n")),
            ("INPUT", String::new()),
        ]);
        let first = transaction(&host, &hooks).unwrap();
        host.private_egress.clear();
        let next = transaction(&host, &hooks).unwrap();
        assert!(first.contains("10.20.30.40"));
        assert!(!next.contains("10.20.30.40"));
        assert!(next.contains(&format!("-F {}", chain(&host, "INPUT"))));
        assert_eq!(next.matches(&jump.replacen("-A ", "-D ", 1)).count(), 2);
        assert!(!next.contains("FOREIGN"));
        assert!(!next.contains("-D DOCKER-USER -s 192.168.0.0/16"));
        assert!(next.contains(&format!(
            "-I INPUT 1 -s {} -j {}",
            host.subnet,
            chain(&host, "INPUT")
        )));
        assert!(!next.contains("-F INPUT\n") && !next.contains("-F DOCKER-USER\n"));
        assert!(next.ends_with("COMMIT\n"));
        let process = Process::recording(std::path::Path::new("."), Recording::default());
        apply(&process, &host).unwrap();
        let recorded = process.recorded().unwrap();
        assert_eq!(
            recorded
                .steps()
                .iter()
                .filter(|step| step.program == "iptables-restore")
                .count(),
            1
        );
        assert!(
            recorded
                .steps()
                .iter()
                .all(|step| step.program == "iptables" || step.program == "iptables-restore")
        );
    }
}
