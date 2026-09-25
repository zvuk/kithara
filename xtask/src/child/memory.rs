use std::time::{Duration, Instant};

#[cfg(unix)]
use anyhow::Context as _;
use anyhow::{Result, ensure};

pub(super) struct Budget {
    limit: u64,
    pub(super) peak: u64,
    sampled: Option<Instant>,
}

impl Budget {
    pub(super) fn new(kib: u64) -> Result<Self> {
        ensure!(cfg!(unix), "process-group RSS sampling requires Unix");
        ensure!(kib > 0, "RSS budget must be positive");
        Ok(Self {
            limit: kib,
            peak: 0,
            sampled: None,
        })
    }

    pub(super) fn check(&mut self, group: u32) -> Result<()> {
        if self
            .sampled
            .is_some_and(|sampled| sampled.elapsed() < Duration::from_millis(200))
        {
            return Ok(());
        }
        // Summed RSS includes shared pages in each process; this is conservative,
        // sampled accounting, not an OS-enforced allocation limit.
        let kib = group_rss(group)?;
        self.sampled = Some(Instant::now());
        self.peak = self.peak.max(kib);
        ensure!(
            kib <= self.limit,
            "owned command RSS {kib} KiB exceeds {} KiB",
            self.limit
        );
        Ok(())
    }
}

#[cfg(unix)]
fn group_rss(group: u32) -> Result<u64> {
    let output = super::output(
        std::process::Command::new("ps").args(["-axo", "pgid=,rss="]),
        None,
        Duration::from_secs(2),
    )?;
    ensure!(
        output.status.success(),
        "process-group RSS sampling failed: {}",
        output.status
    );
    parse_rss(
        std::str::from_utf8(&output.stdout).context("RSS output encoding")?,
        group,
    )
}

#[cfg(not(unix))]
fn group_rss(_group: u32) -> Result<u64> {
    anyhow::bail!("process-group RSS sampling requires Unix")
}

#[cfg(unix)]
fn parse_rss(output: &str, group: u32) -> Result<u64> {
    let mut total = 0_u64;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let pgid: u32 = fields.next().context("missing process group")?.parse()?;
        let rss: u64 = fields.next().context("missing resident size")?.parse()?;
        ensure!(fields.next().is_none(), "unexpected RSS columns");
        if pgid == group {
            total = total.checked_add(rss).context("RSS sum overflow")?;
        }
    }
    Ok(total)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn rss_counts_only_owned_group_and_rejects_invalid_samples() {
        assert_eq!(parse_rss(" 12 4096\n13 8000\n12 2048\n", 12).unwrap(), 6144);
        assert_eq!(parse_rss("13 8000\n", 12).unwrap(), 0);
        for input in [
            "12",
            "12 nope",
            "12 1 extra",
            "12 18446744073709551615\n12 1",
        ] {
            assert!(parse_rss(input, 12).is_err(), "{input}");
        }
    }

    #[test]
    fn rss_limit_stops_only_the_owned_process_group() {
        let mut unrelated = super::super::process::tests::shell("sleep 30");
        let result = super::super::run_bounded(
            std::process::Command::new("sh").args(["-c", "sleep 30 & wait"]),
            None,
            Duration::from_secs(5),
            1,
        );
        let alive = unrelated.try_wait().unwrap().is_none();
        super::super::stop(&mut unrelated, "unrelated test process").unwrap();
        assert!(alive);
        assert!(result.unwrap_err().to_string().contains("RSS"));
    }
}
