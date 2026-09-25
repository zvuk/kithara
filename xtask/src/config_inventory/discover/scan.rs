use anyhow::{Context as _, Result};
use quote::ToTokens;
use serde::Serialize;
use syn::{
    Attribute, Field, FnArg, ImplItemFn, ImplItemType, ItemEnum, ItemFn, ItemImpl, ItemMod,
    ItemStruct, ItemType, LitStr, Meta, Signature, Token, parenthesized,
    parse::Parser as _,
    punctuated::Punctuated,
    visit::{self, Visit},
};

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Registration {
    pub(crate) source: String,
    pub(crate) package: String,
    pub(crate) module_path: String,
    scope: Vec<String>,
    pub(crate) owner: String,
    pub(crate) property: Option<String>,
    pub(crate) hook: Option<String>,
    pub(crate) kind: &'static str,
    pub(crate) sdk: bool,
    pub(crate) docs: Vec<String>,
    conditions: Vec<String>,
    pub(crate) fields: Vec<RegisteredField>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RegisteredField {
    pub(crate) name: String,
    pub(crate) rust_type: String,
    pub(crate) value_type: Option<String>,
    pub(crate) role: String,
    update: bool,
    pub(crate) sdk: bool,
    pub(crate) sdk_max: Option<u32>,
    builder_default: Option<String>,
    exclusion_reason: Option<String>,
    conditions: Vec<String>,
    pub(crate) docs: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Declaration {
    source: String,
    scope: Vec<String>,
    name: String,
    line: usize,
    kind: &'static str,
    conditions: Vec<String>,
    attributes: Vec<String>,
    members: Vec<String>,
}

pub(crate) fn discover(path: &str, source: &str) -> Result<Vec<Declaration>> {
    let syntax =
        syn::parse_file(source).with_context(|| format!("parse config discovery input {path}"))?;
    let mut visitor = Discovery {
        source: path,
        scope: Vec::new(),
        conditions: Vec::new(),
        declarations: Vec::new(),
    };
    visitor.visit_file(&syntax);
    Ok(visitor.declarations)
}

pub(crate) fn registrations(path: &str, source: &str) -> Result<Vec<Registration>> {
    let syntax = syn::parse_file(source)
        .with_context(|| format!("parse config registration input {path}"))?;
    let mut visitor = Registrations {
        source: path,
        package: source_identity(path).0,
        module_path: source_identity(path).1,
        scope: Vec::new(),
        conditions: Vec::new(),
        owner: None,
        registrations: Vec::new(),
        error: None,
    };
    visitor.visit_file(&syntax);
    if let Some(error) = visitor.error {
        return Err(error).with_context(|| format!("parse config registration input {path}"));
    }
    Ok(visitor.registrations)
}

struct Registrations<'a> {
    source: &'a str,
    package: String,
    module_path: String,
    scope: Vec<String>,
    conditions: Vec<String>,
    owner: Option<String>,
    registrations: Vec<Registration>,
    error: Option<anyhow::Error>,
}

fn source_identity(path: &str) -> (String, String) {
    let parts: Vec<_> = path.split('/').collect();
    let (package, source) = match parts.as_slice() {
        ["crates", package, "src", rest @ ..] | ["tests", "crates", package, "src", rest @ ..] => {
            ((*package).to_owned(), rest)
        }
        ["xtask", "src", rest @ ..] => ("xtask".to_owned(), rest),
        ["src", rest @ ..] => ("kithara".to_owned(), rest),
        _ => ("workspace".to_owned(), parts.as_slice()),
    };
    let mut modules: Vec<_> = source
        .iter()
        .map(|part| part.trim_end_matches(".rs"))
        .collect();
    if modules
        .last()
        .is_some_and(|name| matches!(*name, "lib" | "main" | "mod"))
    {
        modules.pop();
    }
    (package, modules.join("::"))
}

fn config_attribute(attrs: &[Attribute]) -> Option<&Attribute> {
    attrs.iter().find(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "config")
    })
}

fn docs(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| match &attr.meta {
            Meta::NameValue(value) => match &value.value {
                syn::Expr::Lit(expr) => match &expr.lit {
                    syn::Lit::Str(text) => Some(text.value().trim().to_owned()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn builder_default(stream: proc_macro2::TokenStream) -> syn::Result<Option<String>> {
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(stream)?;
    Ok(metas.into_iter().find_map(|meta| match meta {
        Meta::Path(path) if path.is_ident("default") => Some("default".to_owned()),
        Meta::NameValue(value) if value.path.is_ident("default") => Some(tokens(&value.value)),
        _ => None,
    }))
}

fn registered_field(field: &Field, snapshot: bool) -> syn::Result<RegisteredField> {
    let name = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(field, "config requires named fields"))?;
    let attr = config_attribute(&field.attrs)
        .ok_or_else(|| syn::Error::new_spanned(field, "registered config field is unclassified"))?;
    let mut role = None;
    let mut projected_type = None;
    let mut update = false;
    let mut sdk = false;
    let mut sdk_max = None;
    let mut default = None;
    let mut exclusion_reason = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("update") {
            update = true;
        } else if meta.path.is_ident("sdk") {
            if sdk {
                return Err(meta.error("duplicate SDK field option"));
            }
            sdk = true;
            if !meta.input.peek(syn::token::Paren) {
                return Ok(());
            }
            meta.parse_nested_meta(|option| {
                if !option.path.is_ident("max") {
                    return Err(option.error("expected sdk(max = positive integer)"));
                }
                let value: syn::LitInt = option.value()?.parse()?;
                let maximum = value.base10_parse::<u32>()?;
                if maximum == 0 {
                    return Err(option.error("SDK maximum must be positive"));
                }
                sdk_max = Some(maximum);
                Ok(())
            })?;
            if sdk_max.is_none() {
                return Err(meta.error("SDK maximum is required"));
            }
        } else if meta.path.is_ident("value") {
            role = Some("value");
            if meta.input.peek(syn::token::Paren) {
                let content;
                parenthesized!(content in meta.input);
                let ty: syn::Type = content.parse()?;
                content.parse::<Token![,]>()?;
                let _: syn::Expr = content.parse()?;
                if !content.is_empty() {
                    return Err(content.error("unexpected projection tokens"));
                }
                projected_type = Some(tokens(&ty));
            }
        } else if meta.path.is_ident("nested") {
            role = Some("nested");
        } else if meta.path.is_ident("skip") {
            role = Some("skip");
            exclusion_reason = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("builder") {
            let content;
            parenthesized!(content in meta.input);
            default = builder_default(content.parse()?)?;
        } else if meta.path.is_ident("field") || meta.path.is_ident("patch") {
            let content;
            parenthesized!(content in meta.input);
            let _: proc_macro2::TokenStream = content.parse()?;
        }
        Ok(())
    })?;
    for attr in &field.attrs {
        if attr.path().is_ident("builder") {
            default = builder_default(attr.meta.require_list()?.tokens.clone())?.or(default);
        }
    }
    let rust_type = tokens(&field.ty);
    let value_type = if snapshot {
        match role {
            Some("value") => Some(projected_type.unwrap_or_else(|| rust_type.clone())),
            Some("nested") => Some(format!("<{rust_type} as kithara_config::Config>::Values")),
            _ => None,
        }
    } else {
        None
    };
    Ok(RegisteredField {
        name: name.to_string(),
        rust_type,
        value_type,
        role: role.unwrap_or("unknown").to_owned(),
        update,
        sdk,
        sdk_max,
        builder_default: default,
        exclusion_reason,
        conditions: conditions(&field.attrs).collect(),
        docs: docs(&field.attrs),
    })
}

impl<'ast> Visit<'ast> for Registrations<'_> {
    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        if let Some(attribute) = config_attribute(&item.attrs) {
            let mut construction = false;
            let parsed = attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("construction") {
                    construction = true;
                } else if meta.path.is_ident("builder") {
                    let _: syn::LitBool = meta.value()?.parse()?;
                } else {
                    return Err(meta.error("unsupported config enum option"));
                }
                Ok(())
            });
            if let Err(error) = parsed {
                self.error = Some(error.into());
                return;
            }
            if !construction {
                self.error = Some(
                    syn::Error::new_spanned(item, "config enum must be construction-only").into(),
                );
                return;
            }
            for variant in &item.variants {
                let mut sdk = false;
                if let Some(attr) = config_attribute(&variant.attrs)
                    && let Err(error) = attr.parse_nested_meta(|meta| {
                        if meta.path.is_ident("sdk") {
                            sdk = true;
                            Ok(())
                        } else {
                            Err(meta.error("construction enum variant supports only sdk"))
                        }
                    })
                {
                    self.error = Some(error.into());
                    return;
                }
                match variant
                    .fields
                    .iter()
                    .map(|field| registered_field(field, false))
                    .collect()
                {
                    Ok(fields) => self.registrations.push(Registration {
                        source: self.source.to_owned(),
                        package: self.package.clone(),
                        module_path: self.module_path.clone(),
                        scope: self.scope.clone(),
                        owner: format!("{}::{}", item.ident, variant.ident),
                        property: None,
                        hook: None,
                        kind: "construction",
                        sdk,
                        docs: docs(&variant.attrs),
                        conditions: self
                            .conditions
                            .iter()
                            .cloned()
                            .chain(conditions(&item.attrs))
                            .chain(conditions(&variant.attrs))
                            .collect(),
                        fields,
                    }),
                    Err(error) => self.error = Some(error.into()),
                }
            }
        }
        visit::visit_item_enum(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let count = self.conditions.len();
        self.conditions.extend(conditions(&item.attrs));
        self.scope.push(item.ident.to_string());
        visit::visit_item_mod(self, item);
        self.scope.pop();
        self.conditions.truncate(count);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if let Some(attribute) = config_attribute(&item.attrs) {
            let mut sdk = false;
            let mut construction = false;
            if matches!(attribute.meta, Meta::List(_))
                && let Err(error) = attribute.parse_nested_meta(|meta| {
                    if meta.path.is_ident("sdk") {
                        sdk = true;
                    } else if meta.path.is_ident("construction") {
                        construction = true;
                    } else if meta.input.peek(syn::Token![=]) {
                        let _: syn::Expr = meta.value()?.parse()?;
                    }
                    Ok(())
                })
            {
                self.error = Some(error.into());
                return;
            }
            match item
                .fields
                .iter()
                .map(|field| registered_field(field, !construction))
                .collect()
            {
                Ok(fields) => self.registrations.push(Registration {
                    source: self.source.to_owned(),
                    package: self.package.clone(),
                    module_path: self.module_path.clone(),
                    scope: self.scope.clone(),
                    owner: item.ident.to_string(),
                    property: None,
                    hook: None,
                    kind: if construction {
                        "construction"
                    } else {
                        "retained"
                    },
                    sdk,
                    docs: docs(&item.attrs),
                    conditions: self
                        .conditions
                        .iter()
                        .cloned()
                        .chain(conditions(&item.attrs))
                        .collect(),
                    fields,
                }),
                Err(error) => self.error = Some(error.into()),
            }
        }
        visit::visit_item_struct(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let count = self.conditions.len();
        self.conditions.extend(conditions(&item.attrs));
        let owner = self.owner.replace(tokens(&item.self_ty));
        self.scope.push(format!("impl {}", tokens(&item.self_ty)));
        visit::visit_item_impl(self, item);
        self.scope.pop();
        self.owner = owner;
        self.conditions.truncate(count);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        let Some(attr) = config_attribute(&item.attrs) else {
            return visit::visit_impl_item_fn(self, item);
        };
        let mut property = None;
        let mut sdk = false;
        if let Err(error) = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("delegate") {
                property = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("sdk") {
                sdk = true;
            }
            Ok(())
        }) {
            self.error = Some(error.into());
            return;
        }
        let fields = item
            .sig
            .inputs
            .iter()
            .filter_map(|arg| match arg {
                FnArg::Typed(input) => Some(RegisteredField {
                    name: match &*input.pat {
                        syn::Pat::Ident(name) => name.ident.to_string(),
                        pattern => tokens(pattern),
                    },
                    rust_type: tokens(&input.ty),
                    value_type: Some(tokens(&input.ty)),
                    role: "delegate_input".to_owned(),
                    update: true,
                    sdk: false,
                    sdk_max: None,
                    builder_default: None,
                    exclusion_reason: None,
                    conditions: conditions(&input.attrs).collect(),
                    docs: Vec::new(),
                }),
                FnArg::Receiver(_) => None,
            })
            .collect();
        self.registrations.push(Registration {
            source: self.source.to_owned(),
            package: self.package.clone(),
            module_path: self.module_path.clone(),
            scope: self.scope.clone(),
            owner: self.owner.clone().unwrap_or_default(),
            property,
            hook: Some(item.sig.ident.to_string()),
            kind: "delegate",
            sdk,
            docs: docs(&item.attrs),
            conditions: self
                .conditions
                .iter()
                .cloned()
                .chain(conditions(&item.attrs))
                .collect(),
            fields,
        });
        visit::visit_impl_item_fn(self, item);
    }
}

struct Discovery<'a> {
    source: &'a str,
    scope: Vec<String>,
    conditions: Vec<String>,
    declarations: Vec<Declaration>,
}

fn tokens(value: &impl ToTokens) -> String {
    value.to_token_stream().to_string()
}

fn conditions(attrs: &[Attribute]) -> impl Iterator<Item = String> + '_ {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr"))
        .map(tokens)
}

fn config_name(name: &str) -> bool {
    [
        "Config",
        "Configuration",
        "Settings",
        "Options",
        "Params",
        "Policy",
        "Setup",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

struct ConfigInput(bool);

impl<'ast> Visit<'ast> for ConfigInput {
    fn visit_type_path(&mut self, ty: &'ast syn::TypePath) {
        self.0 |= ty
            .path
            .segments
            .last()
            .is_some_and(|segment| config_name(&segment.ident.to_string()));
        visit::visit_type_path(self, ty);
    }
}

impl Discovery<'_> {
    fn is_candidate(&self, name: &str, attrs: &[Attribute]) -> bool {
        self.source
            .split('/')
            .any(|part| matches!(part, "config" | "config.rs" | "schema" | "schema.rs"))
            || self
                .scope
                .iter()
                .any(|part| matches!(part.as_str(), "config" | "schema"))
            || config_name(name)
            || attrs.iter().any(|attr| {
                attr.path().is_ident("config")
                    || attr
                        .path()
                        .segments
                        .last()
                        .is_some_and(|segment| segment.ident == "config")
                    || (attr.path().is_ident("derive")
                        && tokens(attr)
                            .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                            .any(|word| matches!(word, "Builder" | "Patch")))
            })
    }
    fn record(
        &mut self,
        name: &syn::Ident,
        kind: &'static str,
        attrs: &[Attribute],
        members: Vec<String>,
    ) {
        self.declarations.push(Declaration {
            source: self.source.to_owned(),
            scope: self.scope.clone(),
            name: name.to_string(),
            line: name.span().start().line,
            kind,
            conditions: self
                .conditions
                .iter()
                .cloned()
                .chain(conditions(attrs))
                .collect(),
            attributes: attrs.iter().map(tokens).collect(),
            members,
        });
    }

    fn constructor(&mut self, signature: &Signature, attrs: &[Attribute]) {
        let builder = attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|part| part.ident == "builder")
        });
        let mut config_input = ConfigInput(false);
        for input in &signature.inputs {
            if let FnArg::Typed(input) = input {
                config_input.visit_type(&input.ty);
            }
        }
        if builder || config_input.0 {
            self.record(
                &signature.ident,
                if builder {
                    "builder_inputs"
                } else {
                    "config_inputs"
                },
                attrs,
                signature
                    .inputs
                    .iter()
                    .filter(|arg| matches!(arg, FnArg::Typed(_)))
                    .map(tokens)
                    .collect(),
            );
        }
    }
}

impl<'ast> Visit<'ast> for Discovery<'_> {
    fn visit_file(&mut self, file: &'ast syn::File) {
        let count = self.conditions.len();
        self.conditions.extend(conditions(&file.attrs));
        visit::visit_file(self, file);
        self.conditions.truncate(count);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if self.is_candidate(&item.ident.to_string(), &item.attrs) {
            self.record(
                &item.ident,
                "struct",
                &item.attrs,
                item.fields.iter().map(tokens).collect(),
            );
        }
        visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        if self.is_candidate(&item.ident.to_string(), &item.attrs) {
            self.record(
                &item.ident,
                "enum",
                &item.attrs,
                item.variants.iter().map(tokens).collect(),
            );
        }
        visit::visit_item_enum(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        if self.is_candidate(&item.ident.to_string(), &item.attrs) {
            self.record(&item.ident, "alias", &item.attrs, vec![tokens(&item.ty)]);
        }
        visit::visit_item_type(self, item);
    }

    fn visit_impl_item_type(&mut self, item: &'ast ImplItemType) {
        if self.is_candidate(&item.ident.to_string(), &item.attrs) {
            self.record(
                &item.ident,
                "associated_alias",
                &item.attrs,
                vec![tokens(&item.ty)],
            );
        }
        visit::visit_impl_item_type(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let count = self.conditions.len();
        self.conditions.extend(conditions(&item.attrs));
        self.scope.push(item.ident.to_string());
        visit::visit_item_mod(self, item);
        self.scope.pop();
        self.conditions.truncate(count);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let count = self.conditions.len();
        self.conditions.extend(conditions(&item.attrs));
        self.scope.push(match &item.trait_ {
            Some((path, _)) => format!("impl {} for {}", tokens(path), tokens(&item.self_ty)),
            None => format!("impl {}", tokens(&item.self_ty)),
        });
        visit::visit_item_impl(self, item);
        self.scope.pop();
        self.conditions.truncate(count);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.constructor(&item.sig, &item.attrs);
        let count = self.conditions.len();
        self.conditions.extend(conditions(&item.attrs));
        self.scope.push(item.sig.ident.to_string());
        visit::visit_item_fn(self, item);
        self.scope.pop();
        self.conditions.truncate(count);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        self.constructor(&item.sig, &item.attrs);
        let count = self.conditions.len();
        self.conditions.extend(conditions(&item.attrs));
        self.scope.push(item.sig.ident.to_string());
        visit::visit_impl_item_fn(self, item);
        self.scope.pop();
        self.conditions.truncate(count);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
