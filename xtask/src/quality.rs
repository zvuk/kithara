use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum QualityCommand {
    /// Generate a quality report.
    Report {
        #[arg(long)]
        min_unimock_traits: Option<usize>,
        #[arg(long)]
        min_rstest_cases: Option<usize>,
        #[arg(long)]
        min_perf_test_files: Option<usize>,
        #[arg(long)]
        min_bench_targets: Option<usize>,
        #[arg(long)]
        max_local_http_servers: Option<usize>,
    },
    /// Audit rstest usage.
    RstestAudit,
    /// Audit trait mocks.
    TraitMockAudit,
    /// List trait-mock exceptions.
    TraitMockExceptions,
    /// Check unimock usage.
    UnimockCheck,
}

pub fn run(cmd: QualityCommand) -> Result<()> {
    let _ = cmd;
    todo!()
}
