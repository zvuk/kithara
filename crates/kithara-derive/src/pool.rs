//! Expansion of `#[derive(Pool)]`.

#![cfg(feature = "pool")]

use proc_macro::TokenStream;
use proc_macro_crate::{Error as ResolverError, FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    Attribute, Data, DeriveInput, Error, Fields, GenericArgument, Ident, PathArguments, Result,
    Type, Visibility, ext::IdentExt, parse_macro_input, spanned::Spanned,
};

mod consts {
    /// Names the generated builder already owns; a field with one would shadow it.
    pub(super) const RESERVED: [&str; 4] = ["builder", "build", "__build_region", "overall_budget"];
    /// The facade crate that re-exports the pool crate.
    pub(super) const FACADE: &str = "kithara";
    /// The crate that owns the pool types.
    pub(super) const POOL: &str = "kithara-bufpool";
}

struct Slot<'a> {
    docs: Vec<&'a Attribute>,
    field: &'a Ident,
    key: &'a Type,
}

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match resolve_owner_path() {
        Ok(owner_path) => derive(&input, &owner_path).unwrap_or_else(Error::into_compile_error),
        Err(error) => error.into_compile_error(),
    }
    .into()
}

/// The path that names the pool owner crate from the compiled crate.
fn resolve_owner_path() -> Result<TokenStream2> {
    owner_path_for(lookup(consts::FACADE), lookup(consts::POOL))
}

/// The dependency lookup for one owner package.
///
/// A missing dependency is an absence. Every other resolver failure is a real
/// error — an unreadable manifest, invalid TOML, a missing manifest directory —
/// and keeps its own context instead of reading as an absent dependency.
fn lookup(package: &str) -> core::result::Result<Option<FoundCrate>, ResolverError> {
    match crate_name(package) {
        Ok(found) => Ok(Some(found)),
        Err(ResolverError::CrateNotFound { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The path that names the pool owner crate from the crates the compiled crate
/// depends on.
///
/// The owner is singular. A crate that depends on the facade reaches the pool
/// types through `kithara::bufpool`; a crate that depends on the pool crate
/// reaches them through `kithara_bufpool`. A crate that depends on both names
/// two owners and cannot say which one it means, and the facade's `bufpool`
/// module is optional, so that manifest is refused rather than guessed.
fn owner_path_for(
    facade: core::result::Result<Option<FoundCrate>, ResolverError>,
    pool: core::result::Result<Option<FoundCrate>, ResolverError>,
) -> Result<TokenStream2> {
    let facade = facade.map_err(|error| resolver_error(consts::FACADE, &error))?;
    let pool = pool.map_err(|error| resolver_error(consts::POOL, &error))?;
    match (facade, pool) {
        // Canonical self-aliases also resolve from separate rustdoc crates.
        (Some(FoundCrate::Itself), _) => Ok(quote!(::kithara::bufpool)),
        // The pool crate names itself through its canonical path, which the
        // owner aliases so the same path works inside it and in its doctests.
        (_, Some(FoundCrate::Itself)) => {
            let owner = Ident::new(&crate_path(consts::POOL), Span::call_site());
            Ok(quote!(::#owner))
        }
        (Some(FoundCrate::Name(_)), Some(FoundCrate::Name(_))) => Err(dual_owner_error()),
        (Some(FoundCrate::Name(facade)), None) => {
            let facade = Ident::new(&facade, Span::call_site());
            Ok(quote!(::#facade::bufpool))
        }
        (None, Some(FoundCrate::Name(pool))) => {
            let pool = Ident::new(&pool, Span::call_site());
            Ok(quote!(::#pool))
        }
        (None, None) => Err(no_owner_error()),
    }
}

/// The canonical Rust path of a package: Cargo's crate-name sanitization.
fn crate_path(package: &str) -> String {
    package.replace('-', "_")
}

/// The compile error for a resolver failure that is not a missing dependency.
fn resolver_error(package: &str, error: &ResolverError) -> Error {
    Error::new(
        Span::call_site(),
        format!("`Pool` could not read the `{package}` dependency: {error}"),
    )
}

/// The compile error for a manifest that names both pool owners.
fn dual_owner_error() -> Error {
    Error::new(
        Span::call_site(),
        "a `Pool` schema needs one pool owner: this crate depends on both the `kithara` \
         facade and `kithara-bufpool`. Keep the facade and import the pool types from \
         `kithara::bufpool`, or keep `kithara-bufpool` and drop the facade.",
    )
}

/// The compile error for a manifest that names no pool owner.
fn no_owner_error() -> Error {
    Error::new(
        Span::call_site(),
        "a `Pool` schema needs the `kithara-bufpool` crate or the `kithara` facade",
    )
}

fn derive(input: &DeriveInput, owner_path: &TokenStream2) -> Result<TokenStream2> {
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &input.generics,
            "`Pool` schemas must be concrete: the generic parameter belongs to `PoolSlot<K>`",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            &input.ident,
            "`Pool` derives only on a struct with named `PoolSlot<K>` fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(
            &input.ident,
            "`Pool` derives only on a struct with named `PoolSlot<K>` fields",
        ));
    };
    if fields.named.is_empty() {
        return Err(Error::new_spanned(
            &input.ident,
            "`Pool` schema must declare at least one `PoolSlot<K>` field",
        ));
    }
    let slots = fields
        .named
        .iter()
        .map(|field| {
            let ident = field
                .ident
                .as_ref()
                .ok_or_else(|| Error::new_spanned(field, "`Pool` schema fields must be named"))?;
            if !matches!(field.vis, Visibility::Inherited) {
                return Err(Error::new_spanned(
                    &field.vis,
                    format!("pool slot `{ident}` must be private to the schema"),
                ));
            }
            let unraw = ident.unraw().to_string();
            if consts::RESERVED.contains(&unraw.as_str()) {
                return Err(Error::new_spanned(
                    ident,
                    format!("pool slot `{unraw}` collides with a generated builder name"),
                ));
            }
            Ok(Slot {
                docs: field
                    .attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("doc"))
                    .collect(),
                field: ident,
                key: slot_key(&field.ty)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let name = &input.ident;
    let context = Ident::new("context", Span::mixed_site());
    let fields: Vec<&Ident> = slots.iter().map(|slot| slot.field).collect();
    let keys: Vec<&Type> = slots.iter().map(|slot| slot.key).collect();
    let params = slots
        .iter()
        .map(|Slot { docs, field, .. }| quote!(#(#docs)* #field: #owner_path::PoolConfig));

    Ok(quote! {
        #[#owner_path::__private::bon::bon(crate = #owner_path::__private::bon)]
        impl #name {
            #[builder(
                builder_type(vis = ""),
                start_fn(name = builder, vis = ""),
                finish_fn(name = build, vis = "")
            )]
            #[doc(hidden)]
            fn __build_region(
                #[builder(start_fn)]
                overall_budget: #owner_path::OverallBudget,
                #(#params,)*
            ) -> ::core::result::Result<
                #owner_path::PoolRegion<Self>,
                #owner_path::PoolError,
            > {
                #owner_path::PoolRegion::__build(overall_budget, |#context| {
                    ::core::result::Result::Ok(Self {
                        #(#fields: #context.slot::<#keys>(#fields)?,)*
                    })
                })
            }
        }

        #(
            impl #owner_path::HasPool<#keys> for #name {
                fn __slot(&self) -> &#owner_path::PoolSlot<#keys> {
                    &self.#fields
                }
            }
        )*
    })
}

/// Return `K` from a field written as a path ending in `PoolSlot<K>`.
fn slot_key(ty: &Type) -> Result<&Type> {
    let refuse = || {
        Error::new(
            ty.span(),
            "pool schema fields must have type `PoolSlot<K>` with one key type",
        )
    };
    let Type::Path(path) = ty else {
        return Err(refuse());
    };
    let segment = path.path.segments.last().ok_or_else(refuse)?;
    if path.qself.is_some() || segment.ident != "PoolSlot" {
        return Err(refuse());
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(refuse());
    };
    let args: Vec<&GenericArgument> = arguments.args.iter().collect();
    match args.as_slice() {
        [GenericArgument::Type(key)] => Ok(key),
        _ => Err(refuse()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use kithara_test_utils::{TestTempDir, kithara};
    use proc_macro_crate::{Error as ResolverError, FoundCrate};
    use proc_macro2::TokenStream as TokenStream2;
    use quote::quote;
    use syn::{DeriveInput, parse_quote};

    use super::{derive, owner_path_for};

    /// A resolved dependency: the package is present in the manifest.
    fn present(name: &str) -> FoundCrate {
        FoundCrate::Name(name.to_owned())
    }

    fn expand(input: &DeriveInput) -> String {
        expand_with(input, &quote!(::kithara_bufpool))
    }

    fn expand_with(input: &DeriveInput, owner_path: &TokenStream2) -> String {
        let output = derive(input, owner_path).expect("valid schema expands");
        syn::parse2::<syn::File>(output.clone()).expect("expansion parses as Rust");
        output.to_string()
    }

    fn refusal(input: &DeriveInput) -> String {
        derive(input, &quote!(::kithara_bufpool))
            .expect_err("schema is refused")
            .to_string()
    }

    #[kithara::test(native)]
    fn expands_one_builder_and_one_capability_per_slot() {
        let output = expand(&parse_quote! {
            /// Pools.
            pub struct AppPools {
                /// Byte pool.
                bytes: PoolSlot<u8>,
                samples: PoolSlot<f32>,
            }
        });
        assert_eq!(output.matches("PoolRegion :: __build (").count(), 1);
        assert!(output.contains("impl :: kithara_bufpool :: HasPool < u8 > for AppPools"));
        assert!(output.contains("impl :: kithara_bufpool :: HasPool < f32 > for AppPools"));
        assert!(
            output.contains("# [doc = r\" Byte pool.\"] bytes : :: kithara_bufpool :: PoolConfig")
        );
        assert!(output.contains("bytes : context . slot :: < u8 > (bytes) ?"));
    }

    #[kithara::test(native)]
    fn names_the_facade_pool_module_when_the_facade_is_the_only_owner() {
        let facade = owner_path_for(Ok(Some(present("kithara"))), Ok(None))
            .expect("the facade names the pool owner");
        assert_eq!(facade.to_string(), ":: kithara :: bufpool");
    }

    #[kithara::test(native)]
    fn names_the_pool_crate_when_the_facade_is_absent() {
        let pool = owner_path_for(Ok(None), Ok(Some(present("kithara_bufpool"))))
            .expect("the pool crate names the pool owner");
        assert_eq!(pool.to_string(), ":: kithara_bufpool");
    }

    #[kithara::test(native)]
    fn names_the_own_crate_from_inside_the_owner() {
        assert_eq!(
            owner_path_for(Ok(Some(FoundCrate::Itself)), Ok(None))
                .expect("the facade names the pool owner")
                .to_string(),
            ":: kithara :: bufpool"
        );
        assert_eq!(
            owner_path_for(Ok(None), Ok(Some(FoundCrate::Itself)))
                .expect("the pool crate names the pool owner")
                .to_string(),
            ":: kithara_bufpool"
        );
    }

    #[kithara::test(native)]
    fn refuses_a_manifest_that_names_both_pool_owners() {
        // The facade's `bufpool` module is optional, so a manifest that names
        // the facade and the pool crate cannot say which owner it means. The
        // shapes that reach this refusal are a direct pool dependency beside a
        // facade without its `bufpool` feature, and a direct pool dependency
        // beside a dev-only or target-inactive facade: the resolver sees the
        // same two owners in all of them.
        let refusal = owner_path_for(
            Ok(Some(present("kithara"))),
            Ok(Some(present("kithara_bufpool"))),
        )
        .expect_err("two owners are ambiguous");
        assert!(
            refusal.to_string().contains("needs one pool owner"),
            "{refusal}"
        );
    }

    #[kithara::test(native)]
    fn refuses_a_schema_without_a_reachable_pool_owner() {
        let refusal = owner_path_for(Ok(None), Ok(None)).expect_err("no pool owner is reachable");
        assert!(
            refusal
                .to_string()
                .contains("needs the `kithara-bufpool` crate or the `kithara` facade"),
            "{refusal}"
        );
    }

    #[kithara::test(native)]
    fn reports_a_resolver_failure_with_its_package_and_cause() {
        let refusal = owner_path_for(Err(ResolverError::CargoManifestDirNotSet), Ok(None))
            .expect_err("the resolver failed");
        let message = refusal.to_string();
        assert!(message.contains("kithara"), "{message}");
        assert!(
            message.contains("CARGO_MANIFEST_DIR"),
            "the underlying cause is kept: {message}"
        );
    }

    const PROBE_SOURCE: &str = "\
struct PoolSlot<K>(core::marker::PhantomData<K>);

#[derive(kithara_derive::Pool)]
pub struct ProbePools {
    bytes: PoolSlot<u8>,
}
";

    fn manifest(name: &str, dependencies: &str) -> String {
        let derive_path = env!("CARGO_MANIFEST_DIR");
        format!(
            "[package]\nname = {name:?}\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
             {dependencies}\n\
             [dependencies.kithara-derive]\npath = {derive_path:?}\n\
             default-features = false\nfeatures = [\"pool\"]\n"
        )
    }

    /// The directory holding the compiled test binary and the derive dylibs.
    fn deps_dir() -> PathBuf {
        std::env::current_exe()
            .expect("test executable path")
            .parent()
            .expect("dependencies directory")
            .to_path_buf()
    }

    #[derive(serde::Deserialize)]
    struct CargoMessage {
        #[serde(default)]
        manifest_path: PathBuf,
        #[serde(default)]
        message: Option<CargoDiagnostic>,
    }

    #[derive(serde::Deserialize)]
    struct CargoDiagnostic {
        rendered: String,
    }

    /// Cargo builds the current derive source, never a cached dylib selected by age.
    fn assert_compile_failures(probes: &[(&str, &str, &str)]) {
        let temp = TestTempDir::new();
        for package in ["kithara", "kithara-bufpool"] {
            let root = temp.path().join(package);
            fs::create_dir_all(root.join("src")).expect("stub package directory");
            fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname = {package:?}\nversion = \"0.0.0\"\n"),
            )
            .expect("stub package manifest");
            fs::write(root.join("src/lib.rs"), "").expect("stub package source");
        }
        let members = probes
            .iter()
            .map(|(name, ..)| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            temp.path().join("Cargo.toml"),
            format!(
                "[workspace]\nmembers = [{members}]\n\
                 exclude = [\"kithara\", \"kithara-bufpool\"]\nresolver = \"3\"\n"
            ),
        )
        .expect("probe workspace manifest");
        for (name, dependencies, _) in probes {
            let dir = temp.path().join(name);
            fs::create_dir_all(dir.join("src")).expect("probe directory");
            fs::write(dir.join("Cargo.toml"), manifest(name, dependencies))
                .expect("probe manifest");
            fs::write(dir.join("src/lib.rs"), PROBE_SOURCE).expect("probe source");
        }

        let output = Command::new(env!("CARGO"))
            .current_dir(temp.path())
            .args([
                "check",
                "--workspace",
                "--offline",
                "--keep-going",
                "--message-format=json",
                "--target-dir",
            ])
            .arg(deps_dir().join("pool-owner-probes"))
            .output()
            .expect("run cargo check");
        assert!(!output.status.success(), "probes unexpectedly compiled");
        let stdout = String::from_utf8(output.stdout).expect("Cargo JSON diagnostics are UTF-8");
        let messages = stdout
            .lines()
            .map(|line| serde_json::from_str::<CargoMessage>(line).expect("Cargo JSON message"))
            .collect::<Vec<_>>();
        for (name, _, expected) in probes {
            let manifest = Path::new(name).join("Cargo.toml");
            let diagnostics = messages
                .iter()
                .filter(|message| message.manifest_path.ends_with(&manifest))
                .filter_map(|message| {
                    message
                        .message
                        .as_ref()
                        .map(|diagnostic| diagnostic.rendered.as_str())
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(diagnostics.contains(expected), "{name}: {diagnostics}");
        }
    }

    #[kithara::test(native)]
    fn pool_owner_manifests_fail_to_compile() {
        assert_compile_failures(&[
            (
                "both-owners",
                "[dependencies]\n\
                 kithara = { path = \"../kithara\" }\n\
                 kithara-bufpool = { path = \"../kithara-bufpool\" }\n",
                "needs one pool owner",
            ),
            (
                "feature-off-facade",
                "[dependencies]\n\
                 kithara = { path = \"../kithara\", default-features = false }\n\
                 kithara-bufpool = { path = \"../kithara-bufpool\" }\n",
                "needs one pool owner",
            ),
            (
                "dev-only-facade",
                "[dependencies]\n\
                 kithara-bufpool = { path = \"../kithara-bufpool\" }\n\n\
                 [dev-dependencies]\n\
                 kithara = { path = \"../kithara\" }\n",
                "needs one pool owner",
            ),
            (
                "target-inactive-facade",
                "[dependencies]\n\
                 kithara-bufpool = { path = \"../kithara-bufpool\" }\n\n\
                 [target.'cfg(windows)'.dependencies]\n\
                 kithara = { path = \"../kithara\" }\n",
                "needs one pool owner",
            ),
            (
                "no-owner",
                "",
                "needs the `kithara-bufpool` crate or the `kithara` facade",
            ),
        ]);
    }

    #[kithara::test(native)]
    fn accepts_qualified_slot_paths_and_raw_identifiers() {
        let output = expand(&parse_quote! {
            struct Pools {
                r#type: kithara_bufpool::PoolSlot<PoolAlias<Tag, VecKey<u8, 1>>>,
                text: ::kithara_bufpool::PoolSlot<TextKey>,
            }
        });
        assert!(output.contains("HasPool < PoolAlias < Tag , VecKey < u8 , 1 > > > for Pools"));
        assert!(output.contains("r#type : context . slot"));
        assert!(output.contains("HasPool < TextKey > for Pools"));
    }

    #[kithara::test(native)]
    #[case::enumeration(parse_quote! { enum Pools { Bytes } }, "named `PoolSlot<K>` fields")]
    #[case::tuple(parse_quote! { struct Pools(PoolSlot<u8>); }, "named `PoolSlot<K>` fields")]
    #[case::unit(parse_quote! { struct Pools; }, "named `PoolSlot<K>` fields")]
    #[case::empty(parse_quote! { struct Pools {} }, "at least one")]
    #[case::generic(parse_quote! { struct Pools<K> { bytes: PoolSlot<K> } }, "concrete")]
    #[case::lifetime(parse_quote! { struct Pools<'a> { bytes: PoolSlot<&'a u8> } }, "concrete")]
    #[case::visible(parse_quote! { struct Pools { pub bytes: PoolSlot<u8> } }, "`bytes` must be private")]
    #[case::scalar(parse_quote! { struct Pools { bytes: u8 } }, "`PoolSlot<K>`")]
    #[case::bare_slot(parse_quote! { struct Pools { bytes: PoolSlot } }, "`PoolSlot<K>`")]
    #[case::two_keys(parse_quote! { struct Pools { bytes: PoolSlot<u8, f32> } }, "`PoolSlot<K>`")]
    #[case::other_wrapper(parse_quote! { struct Pools { bytes: Slot<u8> } }, "`PoolSlot<K>`")]
    #[case::builder(parse_quote! { struct Pools { builder: PoolSlot<u8> } }, "`builder` collides")]
    #[case::build(parse_quote! { struct Pools { build: PoolSlot<u8> } }, "`build` collides")]
    #[case::hidden(parse_quote! { struct Pools { __build_region: PoolSlot<u8> } }, "collides")]
    #[case::budget(parse_quote! { struct Pools { r#overall_budget: PoolSlot<u8> } }, "collides")]
    fn refuses_unsupported_schemas(#[case] input: DeriveInput, #[case] message: &str) {
        let refusal = refusal(&input);
        assert!(refusal.contains(message), "{refusal}");
    }
}
