use anyhow::Result;

use super::{Check, Context, cancel_root_sites::configured_pattern_sites};
use crate::common::violation::Violation;

pub(crate) const ID: &str = "smoothing_primitive_sites";

pub(crate) struct SmoothingPrimitiveSites;

impl Check for SmoothingPrimitiveSites {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.smoothing_primitive_sites;
        configured_pattern_sites(ctx, &cfg.patterns, &cfg.allowed_files, &cfg.exempt_crates).map(
            |sites| {
                sites
                    .into_iter()
                    .map(|site| {
                        Violation::deny(
                        ID,
                        site.location,
                        format!(
                            "hand-rolled parameter smoother `{}` outside the sanctioned gain bank",
                            site.pattern
                        ),
                    )
                    .with_explanation(EXPLANATION)
                    })
                    .collect()
            },
        )
    }
}

const EXPLANATION: &str = "\
Runtime parameters use `SmoothedParam` from `kithara_dsp::param` (firewheel's
type), composed as `MixDSP` for A-to-B transitions. `SmoothingFilter` is
sanctioned only for the equalizer's biquad gain bank and the facade; move
parameter smoothing to the owning config and primitive.";
