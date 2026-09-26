use std::collections::HashMap;

use serde_yaml_ng::Value;

use super::merge::merge;

pub(crate) struct Bake {
    pub(crate) document: String,
    pub(crate) refs: Vec<(String, String)>,
    pub(crate) resolved: Vec<(String, String)>,
}

pub(crate) fn bake(
    target_arch: &str,
    app: &str,
    web: &str,
    env: &HashMap<String, String>,
) -> Result<Bake, serde_yaml_ng::Error> {
    let mut document: Value = serde_yaml_ng::from_str(app)?;
    let is_web = target_arch == "wasm32";
    let text = if is_web {
        merge(&mut document, serde_yaml_ng::from_str(web)?);
        serde_yaml_ng::to_string(&document)?
    } else {
        app.to_owned()
    };
    let mut refs = Vec::new();
    collect_refs(&document, "", &mut refs);
    let resolved = if is_web {
        Vec::new()
    } else {
        let mut names: Vec<&String> = refs.iter().map(|(_, name)| name).collect();
        names.sort_unstable();
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| {
                let value = env.get(name).filter(|value| !value.is_empty())?;
                Some((name.clone(), value.clone()))
            })
            .collect()
    };
    Ok(Bake {
        document: text,
        refs,
        resolved,
    })
}

fn collect_refs(value: &Value, path: &str, refs: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) => {
            if let Some(name) = text.strip_prefix('$').filter(|_| !text.contains("${")) {
                refs.push((path.to_string(), name.to_string()));
                return;
            }
            let mut rest = text.as_str();
            while let Some(start) = rest.find("${") {
                let tail = &rest[start + 2..];
                let Some(end) = tail.find('}') else { break };
                refs.push((path.to_string(), tail[..end].to_string()));
                rest = &tail[end + 1..];
            }
        }
        Value::Sequence(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_refs(item, &format!("{path}[{index}]"), refs);
            }
        }
        Value::Mapping(entries) => {
            for (key, entry) in entries {
                let key = key.as_str().unwrap_or("?");
                let child = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                collect_refs(entry, &child, refs);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_yaml_ng::Value;

    use super::bake;

    mod consts {
        pub(super) const APP: &str = include_str!("../../app.yaml");
        pub(super) const WEB: &str = include_str!("../../app.web.yaml");
        pub(super) const SENTINEL: &str = "kithara-bake-sentinel";
    }

    fn env() -> HashMap<String, String> {
        HashMap::from([(
            "KITHARA_DRM_PROD_KEY".to_owned(),
            consts::SENTINEL.to_owned(),
        )])
    }

    #[kithara::test(native, flash(false))]
    fn a_wasm32_bake_lays_the_overlay_and_resolves_an_empty_table() {
        let baked = bake("wasm32", consts::APP, consts::WEB, &env()).expect("both documents parse");

        let document: Value = serde_yaml_ng::from_str(&baked.document).expect("the bake parses");
        assert_eq!(document["drm"]["providers"], Value::Sequence(Vec::new()));
        assert!(baked.refs.is_empty());
        assert!(baked.resolved.is_empty());
        assert!(!baked.document.contains("KITHARA"));
        assert!(!baked.document.contains(consts::SENTINEL));
    }

    #[kithara::test(native, flash(false))]
    fn a_native_bake_embeds_the_document_verbatim_and_resolves_its_references() {
        let baked =
            bake("aarch64", consts::APP, consts::WEB, &env()).expect("both documents parse");

        assert_eq!(baked.document, consts::APP);
        assert_eq!(
            baked.resolved,
            [(
                "KITHARA_DRM_PROD_KEY".to_owned(),
                consts::SENTINEL.to_owned()
            )]
        );
    }
}
