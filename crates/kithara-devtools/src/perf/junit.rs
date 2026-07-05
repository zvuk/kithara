use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CaseTiming {
    pub(crate) suite: String,
    pub(crate) name: String,
    pub(crate) secs: f64,
    pub(crate) failed: bool,
}

pub(crate) fn parse_junit(xml: &str) -> Result<Vec<CaseTiming>> {
    let doc = roxmltree::Document::parse(xml).context("parse junit xml")?;
    let mut cases = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("testcase")) {
        let name = node.attribute("name").unwrap_or_default().to_owned();
        let suite = node.attribute("classname").unwrap_or_default().to_owned();
        let secs: f64 = node
            .attribute("time")
            .unwrap_or("0")
            .parse()
            .with_context(|| format!("bad time attribute on {suite} {name}"))?;
        let failed = node
            .children()
            .any(|c| c.has_tag_name("failure") || c.has_tag_name("error"));
        cases.push(CaseTiming {
            suite,
            name,
            secs,
            failed,
        });
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="2" failures="1">
  <testsuite name="demo-tests::suite_light" tests="2" failures="1">
    <testcase name="offline::gapless" classname="demo-tests::suite_light" time="1.532"/>
    <testcase name="offline::seek" classname="demo-tests::suite_light" time="0.201">
      <failure type="test failure">boom</failure>
    </testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn parses_cases_and_failures() {
        let cases = parse_junit(XML).expect("parse junit");

        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].suite, "demo-tests::suite_light");
        assert_eq!(cases[0].name, "offline::gapless");
        assert!((cases[0].secs - 1.532).abs() < 1e-9);
        assert!(!cases[0].failed);
        assert!(cases[1].failed);
    }
}
