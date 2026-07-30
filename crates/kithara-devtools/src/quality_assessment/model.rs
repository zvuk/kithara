use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{AssessmentDepth, AssessmentProfile};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Verdict {
    Refactor,
    Investigate,
    EvidenceGap,
    StableWithDebt,
    Healthy,
}

impl Verdict {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Refactor => "refactor",
            Self::Investigate => "investigate",
            Self::EvidenceGap => "evidence-gap",
            Self::StableWithDebt => "stable-with-debt",
            Self::Healthy => "healthy",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum CoverageStatus {
    Executed,
    Reused,
    CoveredBy,
    NotApplicable,
    EvidenceGap,
}

impl CoverageStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::Reused => "reused",
            Self::CoveredBy => "covered-by",
            Self::NotApplicable => "not-applicable",
            Self::EvidenceGap => "evidence-gap",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Scope {
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) loc: u64,
    pub(super) workspace_loc: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Summary {
    pub(super) debt_units: u64,
    pub(super) debt_threshold: u64,
    pub(super) baseline_entries: usize,
    pub(super) findings: usize,
    pub(super) evidence_gaps: usize,
    pub(super) signals: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Signal {
    pub(super) id: String,
    pub(super) tool: String,
    pub(super) scope: String,
    pub(super) category: String,
    pub(super) value: f64,
    pub(super) unit: String,
    pub(super) state: String,
    pub(super) confidence: f64,
    pub(super) evidence_artifacts: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Finding {
    pub(super) id: String,
    pub(super) tool: String,
    pub(super) scope: String,
    pub(super) category: String,
    pub(super) severity: String,
    pub(super) confidence: f64,
    pub(super) debt_units: u64,
    pub(super) locations: Vec<String>,
    pub(super) evidence_artifacts: Vec<String>,
    pub(super) baseline_state: String,
    pub(super) recommended_action: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ToolCoverage {
    pub(super) tool: String,
    pub(super) status: CoverageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) owner: Option<String>,
    pub(super) note: String,
    pub(super) evidence_artifacts: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct BaselineComparison {
    pub(super) source: String,
    pub(super) debt_delta: i64,
    pub(super) finding_delta: i64,
    pub(super) state: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum StageStatus {
    Clean,
    Findings,
    Broken,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum AnalysisStatus {
    Complete,
    Partial,
}

impl AnalysisStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
        }
    }
}

impl StageStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Findings => "findings",
            Self::Broken => "broken",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct StageEvidence {
    pub(super) name: String,
    pub(super) command: Vec<String>,
    pub(super) tools: Vec<String>,
    pub(super) hard_invariant: bool,
    pub(super) status: StageStatus,
    pub(super) exit_code: Option<i32>,
    pub(super) log: String,
    pub(super) evidence_artifacts: Vec<String>,
    pub(super) note: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Assessment {
    pub(super) schema_version: u32,
    pub(super) revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content_digest: Option<String>,
    pub(super) profile: AssessmentProfile,
    pub(super) depth: AssessmentDepth,
    pub(super) scope: Scope,
    pub(super) status: AnalysisStatus,
    pub(super) verdict: Verdict,
    pub(super) summary: Summary,
    pub(super) findings: Vec<Finding>,
    pub(super) signals: Vec<Signal>,
    pub(super) tool_coverage: Vec<ToolCoverage>,
    pub(super) stages: Vec<StageEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) comparison: Option<BaselineComparison>,
    #[serde(skip)]
    pub(super) output_directory: PathBuf,
}
