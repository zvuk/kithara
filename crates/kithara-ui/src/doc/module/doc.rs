use serde::{Deserialize, Serialize};

use super::{binding::BindingRef, node::ControlNode};
#[cfg(test)]
use super::{binding::Priority, style::WindowControlsStyle};
use crate::{
    doc::ron_io,
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, SourceUri},
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ModuleDoc {
    #[serde(default)]
    pub chrome: ChromeStyle,
    pub root: ControlNode,
    pub id: DocId,
    #[serde(default)]
    pub chip: Option<String>,
    #[serde(default)]
    pub drop: Option<ModuleDrop>,
    #[serde(default)]
    pub footer: Option<BindingRef>,
    #[serde(default)]
    pub title: Option<String>,
    pub schema: String,
    #[serde(default)]
    pub assign: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<String>,
    pub version: u32,
}

/// The module takes items dropped on it. The pointer crossing its bounds is
/// reported to the host on `<instance>/drop`; the host holds what is being
/// dragged and runs `write` when the drag ends over the module.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ModuleDrop {
    /// Reads true while a dragged item is over the module.
    pub read: BindingRef,
    pub write: BindingRef,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ChromeStyle {
    Full,
    #[default]
    Frame,
    Plain,
}

/// Parses a validated module document.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope or module body is invalid.
pub fn parse_module(text: &str, origin: &SourceUri) -> Result<ModuleDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Module {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Module.name(),
            found: envelope.kind.name(),
        });
    }
    ron_io::options()
        .from_str(text)
        .map_err(|source| UiDocError::Syntax {
            origin: origin.clone(),
            source: Box::new(source),
        })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::size::{Dim, SizeSpec};

    fn origin() -> SourceUri {
        SourceUri("size.kmodule.ron".to_owned())
    }

    #[kithara::test]
    fn control_size_override_parses() {
        let text = r#"(schema: "kithara.module", version: 1, id: "size",
            root: Button(
                id: "x",
                label: "PLAY",
                size: Some((w: Fixed(40.0), h: Fixed(28.0))),
            ))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Button { size, .. } = document.root else {
            panic!("expected button");
        };

        assert_eq!(
            size,
            Some(SizeSpec::new(Dim::Fixed(40.0), Dim::Fixed(28.0)))
        );
    }

    #[kithara::test]
    fn control_size_defaults_to_none() {
        let text = r#"(schema: "kithara.module", version: 1, id: "size",
            root: Button(id: "x", label: "PLAY"))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Button { size, .. } = document.root else {
            panic!("expected button");
        };

        assert_eq!(size, None);
    }

    #[kithara::test]
    fn crossfader_control_parses_scalar_bindings() {
        let text = r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Crossfader(
                id: "xfade",
                read: Model(id: "mixer.xfade"),
                write: Parameter(id: "mixer.xfade"),
                size: Some((w: Fixed(220.0), h: Fixed(38.0))),
                adaptive: (priority: Required),
            ))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Crossfader {
            read,
            write,
            size,
            adaptive,
            ..
        } = document.root
        else {
            panic!("expected crossfader");
        };

        assert!(matches!(read, Some(BindingRef::Model { .. })));
        assert!(matches!(write, Some(BindingRef::Parameter { .. })));
        assert_eq!(
            size,
            Some(SizeSpec::new(Dim::Fixed(220.0), Dim::Fixed(38.0)))
        );
        assert_eq!(adaptive.priority, Priority::Required);
    }

    #[kithara::test]
    fn tree_control_parses_renderer_bindings() {
        let text = r#"(schema: "kithara.module", version: 1, id: "tree",
            root: Tree(
                id: "browser",
                read: Model(id: "library.tree"),
                query: Model(id: "library.query"),
                size: Some((w: Fixed(232.0), h: Fill)),
                adaptive: (priority: Required),
            ))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Tree {
            read,
            query,
            size,
            adaptive,
            ..
        } = document.root
        else {
            panic!("expected tree");
        };

        assert!(matches!(read, Some(BindingRef::Model { .. })));
        assert!(matches!(query, Some(BindingRef::Model { .. })));
        assert_eq!(size, Some(SizeSpec::new(Dim::Fixed(232.0), Dim::Fill)));
        assert_eq!(adaptive.priority, Priority::Required);
    }

    #[kithara::test]
    fn window_chrome_controls_parse_without_bindings() {
        let text = r#"(schema: "kithara.module", version: 1, id: "window",
            root: Row(children: [
                TitleBar(id: "title", label: "KITHARA"),
                WindowControls(id: "standard", style: Standard),
                WindowControls(id: "compact", style: Compact),
                WindowControls(id: "wide", style: CloseWide),
                WindowControls(id: "micro", style: CloseMicro),
                WindowControls(id: "framed", style: CloseFramed),
            ]))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Row { children, .. } = document.root else {
            panic!("expected row");
        };

        assert!(matches!(children[0], ControlNode::TitleBar { .. }));
        assert!(matches!(
            children[1],
            ControlNode::WindowControls {
                style: WindowControlsStyle::Standard,
                ..
            }
        ));
        assert!(matches!(
            children[2],
            ControlNode::WindowControls {
                style: WindowControlsStyle::Compact,
                ..
            }
        ));
        assert!(matches!(
            children[3],
            ControlNode::WindowControls {
                style: WindowControlsStyle::CloseWide,
                ..
            }
        ));
        assert!(matches!(
            children[4],
            ControlNode::WindowControls {
                style: WindowControlsStyle::CloseMicro,
                ..
            }
        ));
        assert!(matches!(
            children[5],
            ControlNode::WindowControls {
                style: WindowControlsStyle::CloseFramed,
                ..
            }
        ));
    }

    #[kithara::test]
    fn module_chrome_defaults_to_frame() {
        let text = r#"(schema: "kithara.module", version: 1, id: "frame",
            root: Text(id: "label"))"#;

        let document = parse_module(text, &origin()).unwrap();

        assert_eq!(document.title, None);
        assert_eq!(document.chip, None);
        assert!(document.assign.is_empty());
        assert_eq!(document.chrome, ChromeStyle::Frame);
        assert_eq!(document.footer, None);
    }

    #[kithara::test]
    fn full_module_chrome_metadata_parses() {
        let text = r#"(schema: "kithara.module", version: 1, id: "full",
            title: Some("Module title"),
            chip: Some("MOD"),
            assign: ["A", "B"],
            chrome: Full,
            footer: Some(Model(id: "module.status")),
            root: Text(id: "label"))"#;

        let document = parse_module(text, &origin()).unwrap();

        assert_eq!(document.title.as_deref(), Some("Module title"));
        assert_eq!(document.chip.as_deref(), Some("MOD"));
        assert_eq!(document.assign, ["A", "B"]);
        assert_eq!(document.chrome, ChromeStyle::Full);
        assert!(matches!(document.footer, Some(BindingRef::Model { .. })));
    }
}
