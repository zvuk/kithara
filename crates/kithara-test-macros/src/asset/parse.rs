use syn::{
    Error, Ident, LitStr, Token, bracketed,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

use crate::test::case::Case;

/// Case names for one asset function, rejecting the shapes an asset cannot use.
///
/// An unnamed case is numbered by position, so inserting a case ahead of it
/// would silently rename the accessor and re-address the stored bytes. An asset
/// case therefore has to be named.
pub(crate) fn case_names(fn_name: &Ident, cases: &[Case]) -> syn::Result<Vec<String>> {
    if cases.is_empty() {
        return Err(Error::new(
            fn_name.span(),
            "an asset needs at least one `#[case::name(...)]`",
        ));
    }
    let mut names = Vec::with_capacity(cases.len());
    for case in cases {
        let name = case
            .name
            .as_ref()
            .ok_or_else(|| {
                Error::new(
                    fn_name.span(),
                    "an asset case must be named: write `#[case::some_name(...)]`",
                )
            })?
            .to_string();
        if names.contains(&name) {
            return Err(Error::new(
                fn_name.span(),
                format!("duplicate asset case `{name}`"),
            ));
        }
        names.push(name);
    }
    Ok(names)
}

/// Parsed `#[kithara::asset(ext = "…", content_type = "…")]`.
pub(crate) struct AssetArgs {
    pub(crate) content_type: LitStr,
    pub(crate) ext: LitStr,
    /// Cached assets that must be materialized before this producer runs.
    pub(crate) depends_on: Vec<LitStr>,
    /// Environment variables that invalidate this producer.
    pub(crate) env: Vec<LitStr>,
    /// Pass the build context to a required producer.
    pub(crate) context: bool,
    /// Bake the asset into filesystem-free wasm binaries.
    pub(crate) embed: bool,
    /// Keep the build green when this producer reports an unavailable asset.
    pub(crate) optional: bool,
}

impl Parse for AssetArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut content_type: Option<LitStr> = None;
        let mut context = false;
        let mut depends_on: Option<Vec<LitStr>> = None;
        let mut embed = false;
        let mut env: Option<Vec<LitStr>> = None;
        let mut ext: Option<LitStr> = None;
        let mut optional = false;

        while !input.is_empty() {
            let key = input.parse::<Ident>()?;
            let flag = if key == "context" {
                Some(&mut context)
            } else if key == "embed" {
                Some(&mut embed)
            } else if key == "optional" {
                Some(&mut optional)
            } else {
                None
            };
            if let Some(flag) = flag {
                if *flag {
                    return Err(Error::new(key.span(), format!("duplicate key `{key}`")));
                }
                *flag = true;
                if !input.is_empty() {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }
            input.parse::<Token![=]>()?;
            if key == "depends_on" || key == "env" {
                let slot = if key == "depends_on" {
                    &mut depends_on
                } else {
                    &mut env
                };
                if slot.is_some() {
                    return Err(Error::new(key.span(), format!("duplicate key `{key}`")));
                }
                let content;
                bracketed!(content in input);
                let values = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?;
                if values.is_empty() {
                    return Err(content.error(format!("`{key}` needs at least one name")));
                }
                *slot = Some(values.into_iter().collect());
                if !input.is_empty() {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }
            let value = input.parse::<LitStr>()?;
            let slot = match key.to_string().as_str() {
                "content_type" => &mut content_type,
                "ext" => &mut ext,
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!(
                            "unknown asset key `{other}`; expected `ext`, `content_type`, \
                             `depends_on`, `env`, `context`, `embed`, or `optional`"
                        ),
                    ));
                }
            };
            if slot.is_some() {
                return Err(Error::new(key.span(), format!("duplicate key `{key}`")));
            }
            *slot = Some(value);
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        let ext = ext.ok_or_else(|| input.error("asset needs `ext = \"…\"`"))?;
        let content_type =
            content_type.ok_or_else(|| input.error("asset needs `content_type = \"…\"`"))?;

        let ext_value = ext.value();
        if ext_value.is_empty() || ext_value.contains(['/', '\\', '.']) {
            return Err(Error::new(
                ext.span(),
                "asset `ext` must be a bare file extension such as \"wav\"",
            ));
        }
        if embed && optional {
            return Err(input.error("an optional asset cannot be embedded"));
        }

        Ok(Self {
            content_type,
            context,
            embed,
            ext,
            optional,
            depends_on: depends_on.unwrap_or_default(),
            env: env.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetArgs, case_names};
    use crate::test::case::Case;

    #[test]
    fn both_keys_are_parsed() {
        let args = syn::parse_str::<AssetArgs>(r#"ext = "wav", content_type = "audio/wav""#)
            .expect("valid attribute");
        assert_eq!(args.ext.value(), "wav");
        assert_eq!(args.content_type.value(), "audio/wav");
    }

    #[test]
    fn dependencies_are_parsed_in_declaration_order() {
        let args = syn::parse_str::<AssetArgs>(
            r#"ext = "beat", content_type = "application/x-kithara-beat", depends_on = ["rhythm_wav_house_124", "rhythm_score_house_124"]"#,
        )
        .expect("valid attribute");

        assert_eq!(
            args.depends_on
                .iter()
                .map(syn::LitStr::value)
                .collect::<Vec<_>>(),
            ["rhythm_wav_house_124", "rhythm_score_house_124"],
        );
    }

    #[test]
    fn environment_dependencies_are_parsed_in_declaration_order() {
        let args = syn::parse_str::<AssetArgs>(
            r#"ext = "toml", content_type = "application/toml", env = ["TOKEN", "KEY"]"#,
        )
        .expect("valid attribute");

        assert_eq!(
            args.env.iter().map(syn::LitStr::value).collect::<Vec<_>>(),
            ["TOKEN", "KEY"],
        );
    }

    #[test]
    fn key_order_does_not_matter() {
        let args = syn::parse_str::<AssetArgs>(r#"content_type = "audio/mpeg", ext = "mp3""#)
            .expect("valid attribute");
        assert_eq!(args.ext.value(), "mp3");
        assert_eq!(args.content_type.value(), "audio/mpeg");
    }

    #[test]
    fn a_missing_key_is_rejected() {
        for input in [r#"ext = "wav""#, r#"content_type = "audio/wav""#, ""] {
            assert!(
                syn::parse_str::<AssetArgs>(input).is_err(),
                "accepted {input}",
            );
        }
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        assert!(
            syn::parse_str::<AssetArgs>(r#"ext = "wav", content_type = "audio/wav", cache = true"#)
                .is_err(),
        );
    }

    #[test]
    fn a_duplicate_key_is_rejected() {
        assert!(
            syn::parse_str::<AssetArgs>(r#"ext = "wav", ext = "mp3", content_type = "audio/wav""#)
                .is_err(),
        );
    }

    #[test]
    fn embed_is_off_by_default_and_opt_in() {
        let plain = syn::parse_str::<AssetArgs>(r#"ext = "wav", content_type = "audio/wav""#)
            .expect("valid attribute");
        assert!(!plain.embed);

        let embedded =
            syn::parse_str::<AssetArgs>(r#"ext = "wav", content_type = "audio/wav", embed"#)
                .expect("valid attribute");
        assert!(embedded.embed);
    }

    #[test]
    fn optional_is_off_by_default_and_opt_in() {
        let plain = syn::parse_str::<AssetArgs>(r#"ext = "wav", content_type = "audio/wav""#)
            .expect("valid attribute");
        assert!(!plain.optional);

        let optional = syn::parse_str::<AssetArgs>(
            r#"ext = "m3u8", content_type = "application/vnd.apple.mpegurl", optional"#,
        )
        .expect("valid attribute");
        assert!(optional.optional);
    }

    #[test]
    fn context_is_off_by_default_and_opt_in() {
        let plain = syn::parse_str::<AssetArgs>(r#"ext = "wav", content_type = "audio/wav""#)
            .expect("valid attribute");
        assert!(!plain.context);

        let contextual = syn::parse_str::<AssetArgs>(
            r#"ext = "toml", content_type = "application/toml", context"#,
        )
        .expect("valid attribute");
        assert!(contextual.context);
    }

    #[test]
    fn embed_is_rejected_twice() {
        assert!(
            syn::parse_str::<AssetArgs>(r#"ext = "wav", content_type = "audio/wav", embed, embed"#)
                .is_err(),
        );
    }

    #[test]
    fn optional_asset_cannot_be_embedded() {
        assert!(
            syn::parse_str::<AssetArgs>(
                r#"ext = "m3u8", content_type = "application/vnd.apple.mpegurl", optional, embed"#
            )
            .is_err(),
        );
    }

    #[test]
    fn an_extension_with_a_path_separator_is_rejected() {
        for ext in ["../escape", "sub/dir", ""] {
            let input = format!(r#"ext = "{ext}", content_type = "audio/wav""#);
            assert!(
                syn::parse_str::<AssetArgs>(&input).is_err(),
                "accepted {ext}"
            );
        }
    }

    fn ident(name: &str) -> syn::Ident {
        syn::parse_str::<syn::Ident>(name).expect("valid identifier")
    }

    fn named_case(name: &str) -> Case {
        Case {
            name: Some(ident(name)),
            values: vec![],
        }
    }

    #[test]
    fn case_names_keep_declaration_order() {
        let names = case_names(
            &ident("sine_wav"),
            &[named_case("a440"), named_case("a880")],
        )
        .expect("valid cases");
        assert_eq!(names, ["a440", "a880"]);
    }

    #[test]
    fn an_unnamed_case_is_rejected() {
        let unnamed = Case {
            name: None,
            values: vec![],
        };
        assert!(case_names(&ident("sine_wav"), &[unnamed]).is_err());
    }

    #[test]
    fn a_repeated_case_name_is_rejected() {
        assert!(
            case_names(
                &ident("sine_wav"),
                &[named_case("a440"), named_case("a440")]
            )
            .is_err(),
        );
    }

    #[test]
    fn an_empty_case_matrix_is_rejected() {
        assert!(case_names(&ident("sine_wav"), &[]).is_err());
    }
}
