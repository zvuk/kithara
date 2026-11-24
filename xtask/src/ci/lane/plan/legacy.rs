use std::{collections::BTreeMap, slice};

use anyhow::{Context, Result};
use clap::ValueEnum as _;
use serde_json::{Value, json};

use super::catalog::{commands, digest};
use crate::{
    ci::{
        lane::select::{self, Fleet, LanesArgs},
        run::PipelineKind,
    },
    config::CiProjectConfig,
    consts,
};

pub(super) fn fingerprints(config: &CiProjectConfig) -> Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for kind in PipelineKind::value_variants() {
        for role in consts::LANE_ROLES {
            for fleet in [Fleet::Github, Fleet::Gitlab] {
                let mut cases = Vec::new();
                for paths in [
                    None,
                    Some(Vec::new()),
                    Some(vec!["README.md".to_owned()]),
                    Some(vec!["Cargo.lock".to_owned()]),
                ] {
                    cases.push(capture(config, role, *kind, fleet, &[], paths.as_deref())?);
                }
                cases.push(capture(
                    config,
                    role,
                    *kind,
                    fleet,
                    &["all".to_owned()],
                    None,
                )?);
                for (name, lane) in &config.lanes {
                    if lane.role == role {
                        cases.push(capture(
                            config,
                            role,
                            *kind,
                            fleet,
                            slice::from_ref(name),
                            None,
                        )?);
                    }
                }
                let key = format!("{}:{role}:{fleet:?}", kind.name());
                output.insert(key, digest(&serde_json::to_vec(&cases)?));
            }
        }
    }
    Ok(output)
}

fn capture(
    config: &CiProjectConfig,
    role: &str,
    kind: PipelineKind,
    fleet: Fleet,
    only: &[String],
    paths: Option<&[String]>,
) -> Result<Value> {
    let args = LanesArgs {
        role: role.to_owned(),
        kind,
        only: only.to_owned(),
        fleet,
        field: None,
        base: None,
        changed_paths: paths.map(ToOwned::to_owned),
    };
    let result = match select::render(&config.lanes, &args) {
        Ok(selection) => {
            let mut contracts = BTreeMap::new();
            for id in selection
                .matrix
                .iter()
                .map(|entry| &entry.lane)
                .chain(selection.dependent.iter().map(|entry| &entry.lane))
            {
                let mut lane = serde_json::to_value(&config.lanes[id])?;
                lane.as_object_mut()
                    .context("serialized lane is an object")?
                    .remove("semantics");
                lane["steps"] = commands(&config.lanes[id], kind.name());
                contracts.insert(id.clone(), lane);
            }
            json!({"selection":selection,"contracts":contracts})
        }
        Err(error) => json!({"refused":format!("{error:#}")}),
    };
    Ok(json!({"only":only,"changed_paths":paths,"result":result}))
}
