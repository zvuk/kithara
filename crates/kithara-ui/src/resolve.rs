use std::collections::{BTreeMap, BTreeSet};

use crate::{
    consts::CONTENT,
    error::UiDocError,
    ids::SourceUri,
    module::{ControlNode, Include, ModuleDoc, parse_module},
    source::{FillDocument, Limits, LoadedModule, LoadedSource, ModuleSource, SourceResolver},
    validate,
};

#[derive(Debug, Default)]
pub(crate) struct ModuleSet {
    pub(crate) defs: BTreeMap<SourceUri, ModuleDoc>,
    /// Every shader a module declares, under the document that declared it.
    /// Nested rather than keyed by a pair so that a lookup borrows both halves
    /// of the key instead of building one.
    shaders: BTreeMap<SourceUri, BTreeMap<String, LoadedSource>>,
    pub(crate) collections: BTreeMap<String, Vec<Filled>>,
    holders: BTreeSet<SourceUri>,
}

pub(crate) fn collection_address(module: &str, from: &str) -> String {
    format!("{module}/{from}")
}

#[derive(Debug)]
pub(crate) struct Filled {
    pub(crate) key: String,
    pub(crate) uri: SourceUri,
}

impl ModuleSet {
    pub(crate) fn def(&self, uri: &SourceUri) -> Result<&ModuleDoc, UiDocError> {
        self.defs.get(uri).ok_or_else(|| UiDocError::NotFound {
            origin: uri.clone(),
            rel: uri.0.clone(),
        })
    }

    pub(crate) fn shader(&self, origin: &SourceUri, source: &str) -> Option<&LoadedSource> {
        self.shaders.get(origin)?.get(source)
    }
}

pub(crate) fn load_module_graph(
    resolver: &dyn SourceResolver,
    base: Option<&SourceUri>,
    rel: &str,
    limits: &Limits,
) -> Result<(SourceUri, ModuleSet), UiDocError> {
    let mut loader = Loader {
        resolver,
        limits,
        set: ModuleSet::default(),
        stack: Vec::new(),
    };
    let uri = loader.load(base, rel, 0)?;
    loader.mount(&uri)?;
    Ok((uri, loader.set))
}

struct Loader<'a> {
    resolver: &'a dyn SourceResolver,
    limits: &'a Limits,
    set: ModuleSet,
    stack: Vec<SourceUri>,
}

impl Loader<'_> {
    fn load(
        &mut self,
        base: Option<&SourceUri>,
        rel: &str,
        depth: usize,
    ) -> Result<SourceUri, UiDocError> {
        let loaded = self.resolver.module(base, rel)?;
        self.enter(loaded, depth)
    }

    fn mount(&self, uri: &SourceUri) -> Result<(), UiDocError> {
        validate::check_module_root(self.set.def(uri)?, uri)
    }

    fn parsed(
        &mut self,
        origin: &SourceUri,
        document: &ModuleDoc,
        depth: usize,
    ) -> Result<SourceUri, UiDocError> {
        let loaded = LoadedModule {
            uri: origin.clone(),
            source: ModuleSource::Document(Box::new(document.clone())),
        };
        self.enter(loaded, depth)
    }

    fn enter(&mut self, loaded: LoadedModule, depth: usize) -> Result<SourceUri, UiDocError> {
        if self.stack.contains(&loaded.uri) {
            let mut chain = self.stack.clone();
            chain.push(loaded.uri);
            return Err(UiDocError::IncludeCycle { chain });
        }
        if depth >= self.limits.max_depth {
            return Err(UiDocError::DepthExceeded {
                depth,
                origin: loaded.uri,
                max: self.limits.max_depth,
            });
        }
        if self.set.defs.contains_key(&loaded.uri) {
            return Ok(loaded.uri);
        }
        let doc = match loaded.source {
            ModuleSource::Text(text) => {
                if text.len() > self.limits.max_bytes {
                    return Err(UiDocError::TooLarge {
                        bytes: text.len(),
                        origin: loaded.uri,
                        max: self.limits.max_bytes,
                    });
                }
                parse_module(&text, &loaded.uri)?
            }
            ModuleSource::Document(doc) => {
                doc.check(&loaded.uri)?;
                *doc
            }
        };
        validate::check_module_id(&doc, &loaded.uri)?;
        validate::check_module_node_ids(&doc, &loaded.uri)?;
        self.stack.push(loaded.uri.clone());
        self.walk(&loaded.uri, &doc.id.0, &doc.root, depth)?;
        let popped = self.stack.pop();
        debug_assert_eq!(popped.as_ref(), Some(&loaded.uri));
        self.set.defs.insert(loaded.uri.clone(), doc);
        Ok(loaded.uri)
    }

    fn collection(
        &mut self,
        origin: &SourceUri,
        address: String,
        each: Option<&Include>,
        depth: usize,
    ) -> Result<(), UiDocError> {
        if let Some(each) = each {
            let template = self.load(Some(origin), &each.source, depth + 1)?;
            if !self.set.holders.contains(&template) {
                return Err(UiDocError::TemplateWithoutContent { origin: template });
            }
        }
        if self.set.collections.contains_key(&address) {
            return Ok(());
        }
        let resolver = self.resolver;
        let mut filled: Vec<Filled> = Vec::new();
        for fill in resolver.fills() {
            if fill.address == address {
                let uri = match &fill.document {
                    FillDocument::Parsed { document, origin } => {
                        self.parsed(origin, document, depth + 1)?
                    }
                    FillDocument::Path(path) => self.load(None, path, depth + 1)?,
                };
                self.mount(&uri)?;
                filled.push(Filled {
                    uri,
                    key: fill.key.clone(),
                });
            }
        }
        self.set.collections.insert(address, filled);
        Ok(())
    }

    fn walk(
        &mut self,
        origin: &SourceUri,
        module: &str,
        node: &ControlNode,
        depth: usize,
    ) -> Result<(), UiDocError> {
        match node {
            ControlNode::Row { children, .. }
            | ControlNode::Column { children, .. }
            | ControlNode::Stage { children, .. } => {
                for child in children {
                    self.walk(origin, module, child, depth)?;
                }
                Ok(())
            }
            ControlNode::Slot {
                id,
                default,
                from,
                each,
                select,
                ..
            } => {
                if id.0 == CONTENT {
                    self.set.holders.insert(origin.clone());
                }
                match (from, each, select) {
                    (Some(from), each, select) if each.is_some() != select.is_some() => {
                        let address = collection_address(module, from);
                        self.collection(origin, address, each.as_ref(), depth)?;
                    }
                    (None, None, None) => {}
                    _ => {
                        return Err(UiDocError::CollectionShape {
                            origin: origin.clone(),
                            slot: id.0.clone(),
                        });
                    }
                }
                for child in default {
                    self.walk(origin, module, child, depth)?;
                }
                Ok(())
            }
            ControlNode::Adaptive { base, steps, .. } => {
                self.walk(origin, module, base, depth)?;
                for step in steps {
                    self.walk(origin, module, &step.node, depth)?;
                }
                Ok(())
            }
            ControlNode::Object { child, .. }
            | ControlNode::Optional { child, .. }
            | ControlNode::Placed { child, .. }
            | ControlNode::Pressable { child, .. }
            | ControlNode::Reveal { child, .. }
            | ControlNode::Scroll { child, .. }
            | ControlNode::Modal { content: child, .. } => self.walk(origin, module, child, depth),
            ControlNode::Popover {
                anchor, content, ..
            } => {
                self.walk(origin, module, anchor, depth)?;
                self.walk(origin, module, content, depth)
            }
            ControlNode::Include { source, .. } => {
                let uri = self.load(Some(origin), source, depth + 1)?;
                self.mount(&uri)
            }
            ControlNode::Shader { source, .. } => {
                if self.set.shader(origin, source).is_none() {
                    let loaded = load_source(self.resolver, Some(origin), source, self.limits)?;
                    self.set
                        .shaders
                        .entry(origin.clone())
                        .or_default()
                        .insert(source.clone(), loaded);
                }
                Ok(())
            }
            ControlNode::DeckSummary { .. }
            | ControlNode::Brand { .. }
            | ControlNode::Spacer { .. }
            | ControlNode::Divider { .. }
            | ControlNode::PresetSelector { .. }
            | ControlNode::SettingsButton { .. }
            | ControlNode::WindowDrag { .. }
            | ControlNode::TitleBar { .. }
            | ControlNode::WindowControls { .. }
            | ControlNode::Text { .. }
            | ControlNode::Glyph { .. }
            | ControlNode::NavItem { .. }
            | ControlNode::TabLarge { .. }
            | ControlNode::Button { .. }
            | ControlNode::Bpm { .. }
            | ControlNode::Time { .. }
            | ControlNode::Scalar { .. }
            | ControlNode::Crossfader { .. }
            | ControlNode::Fader { .. }
            | ControlNode::Wave { .. }
            | ControlNode::Vis { .. }
            | ControlNode::Lottie { .. }
            | ControlNode::Sprite { .. }
            | ControlNode::Custom { .. }
            | ControlNode::PortalMap { .. }
            | ControlNode::Range { .. }
            | ControlNode::Table { .. }
            | ControlNode::Search { .. }
            | ControlNode::Tree { .. }
            | ControlNode::ContextBar { .. }
            | ControlNode::Toggle { .. }
            | ControlNode::Checkbox { .. }
            | ControlNode::Segmented { .. }
            | ControlNode::Select { .. }
            | ControlNode::StatusDot { .. }
            | ControlNode::Swatch { .. }
            | ControlNode::Cell { .. }
            | ControlNode::Readout { .. }
            | ControlNode::Chip { .. }
            | ControlNode::Knob { .. }
            | ControlNode::VuStereo { .. }
            | ControlNode::VuVertical { .. }
            | ControlNode::Meter { .. } => Ok(()),
        }
    }
}

pub(crate) fn load_source(
    resolver: &dyn SourceResolver,
    base: Option<&SourceUri>,
    rel: &str,
    limits: &Limits,
) -> Result<LoadedSource, UiDocError> {
    let loaded = resolver.load(base, rel)?;
    let bytes = loaded.text.len();
    if bytes > limits.max_bytes {
        return Err(UiDocError::TooLarge {
            bytes,
            origin: loaded.uri,
            max: limits.max_bytes,
        });
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::source::{Fill, Limits, LoadedBytes, MemResolver, resolve_uri};

    fn module(id: &str, body: &str) -> String {
        format!(r#"(schema: "kithara.module", version: 1, id: "{id}", root: {body})"#)
    }

    /// Ready documents by path, over the sources of `text`.
    #[derive(Default)]
    struct Ready {
        documents: BTreeMap<String, ModuleDoc>,
        text: MemResolver,
    }

    impl Ready {
        fn with(path: &str, document: ModuleDoc) -> Self {
            let mut ready = Self::default();
            ready.documents.insert(path.to_owned(), document);
            ready
        }
    }

    impl SourceResolver for Ready {
        fn fills(&self) -> Vec<&Fill> {
            Vec::new()
        }

        fn module(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedModule, UiDocError> {
            let uri = resolve_uri(base, rel)?;
            self.documents.get(&uri.0).map_or_else(
                || self.text.module(base, rel),
                |document| {
                    Ok(LoadedModule {
                        uri,
                        source: ModuleSource::Document(Box::new(document.clone())),
                    })
                },
            )
        }

        delegate::delegate! {
            to self.text {
                fn bytes(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedBytes, UiDocError>;
                fn load(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedSource, UiDocError>;
            }
        }
    }

    #[kithara::test]
    fn ready_modules_share_envelope_validation_with_text() {
        for (schema, version) in [
            ("unknown", 1),
            ("kithara.module", 0),
            ("kithara.module", 2),
            ("kithara.layout", 1),
        ] {
            let origin = SourceUri("ready.kmodule.ron".to_owned());
            let mut doc = ModuleDoc::new(
                crate::ids::DocId("ready".to_owned()),
                ControlNode::Spacer {
                    id: crate::ids::NodeId("body".to_owned()),
                    size: None,
                    read: None,
                    write: None,
                },
            );
            doc.schema = schema.to_owned();
            doc.version = version;
            let text = format!(
                r#"(schema: "{schema}", version: {version}, id: "ready", root: Spacer(id: "body"))"#
            );
            let expected = parse_module(&text, &origin).unwrap_err();
            let resolver = Ready::with(&origin.0, doc);
            let actual =
                load_module_graph(&resolver, None, &origin.0, &Limits::default()).unwrap_err();
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
            assert_eq!(actual.to_string(), expected.to_string());
        }
    }

    #[kithara::test]
    fn ready_modules_follow_relative_includes_through_package_overlays() {
        let ready = Ready::with(
            "sub/a.kmodule.ron",
            ModuleDoc::new(
                crate::ids::DocId("a".to_owned()),
                ControlNode::Include {
                    id: crate::ids::NodeId("b".to_owned()),
                    source: "b.kmodule.ron".to_owned(),
                    with: BTreeMap::new(),
                },
            ),
        );
        let mut text = MemResolver::default();
        text.insert("sub/b.kmodule.ron", &module("b", r#"Spacer(id: "body")"#));
        let resolver = crate::source::OverlayResolver::new(ready, text);
        let (_, set) =
            load_module_graph(&resolver, None, "sub/a.kmodule.ron", &Limits::default()).unwrap();
        assert_eq!(set.defs.len(), 2);
        assert!(
            set.defs
                .contains_key(&SourceUri("sub/b.kmodule.ron".to_owned()))
        );
    }

    #[kithara::test]
    fn ready_modules_share_include_cycle_detection_with_text() {
        let mut resolver = Ready::with(
            "a.kmodule.ron",
            ModuleDoc::new(
                crate::ids::DocId("a".to_owned()),
                ControlNode::Include {
                    id: crate::ids::NodeId("b".to_owned()),
                    source: "b.kmodule.ron".to_owned(),
                    with: BTreeMap::new(),
                },
            ),
        );
        resolver.text.insert(
            "b.kmodule.ron",
            &module("b", r#"Include(id: "a", source: "a.kmodule.ron")"#),
        );
        let error =
            load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap_err();
        assert!(
            matches!(error, UiDocError::IncludeCycle { chain } if chain == ["a.kmodule.ron", "b.kmodule.ron", "a.kmodule.ron"].map(|uri| SourceUri(uri.to_owned())))
        );
    }

    #[kithara::test]
    fn loads_nested_includes_two_levels_deep() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [Include(id: "b", source: "sub/b.kmodule.ron")])"#,
            ),
        );
        resolver.insert(
            "sub/b.kmodule.ron",
            &module(
                "b",
                r#"Row(children: [Include(id: "c", source: "c.kmodule.ron")])"#,
            ),
        );
        resolver.insert("sub/c.kmodule.ron", &module("c", r#"Text(id: "x")"#));

        let (uri, set) =
            load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap();
        assert_eq!(uri.0, "a.kmodule.ron");
        assert_eq!(set.defs.len(), 3);
    }

    #[kithara::test]
    fn include_cycle_reports_full_chain() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [Include(id: "b", source: "b.kmodule.ron")])"#,
            ),
        );
        resolver.insert(
            "b.kmodule.ron",
            &module(
                "b",
                r#"Row(children: [Include(id: "a", source: "a.kmodule.ron")])"#,
            ),
        );

        let error =
            load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("a.kmodule.ron -> b.kmodule.ron -> a.kmodule.ron"),
            "{message}"
        );
    }

    #[kithara::test]
    fn depth_limit_is_enforced() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [Include(id: "b", source: "b.kmodule.ron")])"#,
            ),
        );
        resolver.insert("b.kmodule.ron", &module("b", r#"Text(id: "x")"#));

        let limits = Limits {
            max_depth: 1,
            ..Limits::default()
        };
        let error = load_module_graph(&resolver, None, "a.kmodule.ron", &limits).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::DepthExceeded {
                depth: 1,
                max: 1,
                ..
            }
        ));
    }

    #[kithara::test]
    fn shared_include_is_loaded_once_not_a_cycle() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [
                    Include(id: "left", source: "shared.kmodule.ron"),
                    Include(id: "right", source: "shared.kmodule.ron"),
                ])"#,
            ),
        );
        resolver.insert("shared.kmodule.ron", &module("shared", r#"Text(id: "x")"#));

        load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap();
    }

    #[kithara::test]
    fn a_template_reached_again_as_a_module_is_held_to_a_module_root() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [
                    Slot(id: "items", from: "items", each: Include(source: "item.kmodule.ron")),
                    Include(id: "plain", source: "item.kmodule.ron"),
                ])"#,
            ),
        );
        resolver.insert(
            "item.kmodule.ron",
            &module(
                "item",
                r#"Optional(id: "frame", hidden: Model(id: "x"), child: Slot(id: "content"))"#,
            ),
        );

        let error =
            load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap_err();

        assert!(matches!(error, UiDocError::RootBlock { .. }), "{error}");
    }

    #[kithara::test]
    fn oversized_included_source_is_rejected() {
        let entry = module("a", r#"Include(id: "b", source: "b.kmodule.ron")"#);
        let child = module(
            "b",
            &format!(r#"Chip(id: "text", label: "{}")"#, "x".repeat(256)),
        );
        assert!(child.len() > entry.len());
        let mut resolver = MemResolver::default();
        resolver.insert("a.kmodule.ron", &entry);
        resolver.insert("b.kmodule.ron", &child);
        let limits = Limits {
            max_bytes: entry.len(),
            ..Limits::default()
        };

        let error = load_module_graph(&resolver, None, "a.kmodule.ron", &limits).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::TooLarge { origin, bytes, max }
                if origin.0 == "b.kmodule.ron" && bytes == child.len() && max == entry.len()
        ));
    }
}
