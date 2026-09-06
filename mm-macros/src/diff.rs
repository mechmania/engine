//! Derive macro for the `Diff` trait.
//!
//! `Diff` turns a pair of values of the same type into a sparse JSON object holding
//! the new value of each field that changed -- `{ "health": 7.5 }` -- which a
//! consumer merges forward onto a running state. Entirely mechanical to write by
//! hand; this macro generates it.
//!
//! ```ignore
//! #[derive(Diff)]
//! struct GameState {
//!     #[diff(always)] tick: u32,          // emitted every time, changed or not
//!     #[diff(nested)] fleet_a: BotArray,  // recurses into `BotArray::diff_json`
//!     #[diff(skip)]   scratch: Vec<u8>,   // omitted entirely
//!     health: f32,                        // leaf (default): emitted when it changes
//! }
//! ```
//!
//! Fields are leaves by default, which requires `PartialEq + Serialize` on the field
//! type. There is no way to blanket-impl `Diff` for every `PartialEq + Serialize`
//! type and still have derived impls -- the two overlap and coherence rejects it --
//! so recursion is opt-in per field rather than inferred.
//!
//! `always` does **not** defeat the `a == b` early return: two fully equal values
//! still diff to `None`, so an `always` field never resurrects an otherwise
//! unchanged value. That is what keeps unchanged bots out of `BotArray`'s `changed`
//! map -- without it, marking `BotState::id` as `always` would flood every tick with
//! all 32 live slots.

use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, Path};

/// How a single field contributes to the diff.
enum FieldMode {
    /// Compare with `!=` and emit the new value.
    Leaf,
    /// Emit the value every time, changed or not.
    Always,
    /// Recurse via the field type's own `Diff` impl.
    Nested,
    /// Leave the field out of the diff.
    Skip,
}

pub(crate) fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let trait_path = container_trait_path(&input)?;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(Error::new_spanned(
                    &input.ident,
                    "Diff can only be derived for structs with named fields",
                ))
            }
        },
        Data::Enum(_) | Data::Union(_) => {
            return Err(Error::new_spanned(
                &input.ident,
                "Diff can only be derived for structs, not enums or unions \
                 (an enum is usually better handled as a leaf field)",
            ))
        }
    };

    let mut stmts = Vec::new();
    for field in fields {
        // named fields, checked above
        let name = field.ident.as_ref().unwrap();
        let key = name.to_string();
        let ty = &field.ty;

        match field_mode(field)? {
            FieldMode::Skip => {}
            FieldMode::Leaf => stmts.push(quote! {
                if a.#name != b.#name {
                    res.insert(
                        ::std::string::String::from(#key),
                        ::serde_json::json!(&b.#name),
                    );
                }
            }),
            FieldMode::Always => stmts.push(quote! {
                res.insert(
                    ::std::string::String::from(#key),
                    ::serde_json::json!(&b.#name),
                );
            }),
            FieldMode::Nested => stmts.push(quote! {
                if let ::std::option::Option::Some(d) =
                    <#ty as #trait_path>::diff_json(&a.#name, &b.#name)
                {
                    res.insert(::std::string::String::from(#key), d);
                }
            }),
        }
    }

    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics #trait_path for #ident #ty_generics #where_clause {
            fn diff_json(
                a: &Self,
                b: &Self,
            ) -> ::std::option::Option<::serde_json::Value> {
                if a == b {
                    return ::std::option::Option::None;
                }

                let mut res = ::serde_json::Map::new();
                #(#stmts)*

                // `a != b` can still leave `res` empty when every field that
                // differs is `skip`ped. An empty object carries no information,
                // so report it as "no diff" rather than emitting `{}`. This is a
                // no-op for a type with an `always` field, which always inserts.
                if res.is_empty() {
                    return ::std::option::Option::None;
                }
                ::std::option::Option::Some(::serde_json::Value::Object(res))
            }
        }
    })
}

/// Path to the `Diff` trait. Defaults to where it lives in `mm-engine`; override
/// with `#[diff(trait = "some::other::Diff")]` when deriving from another crate.
fn container_trait_path(input: &DeriveInput) -> syn::Result<Path> {
    let mut path: Path = syn::parse_quote!(crate::game::diff::Diff);

    for attr in &input.attrs {
        if !attr.path().is_ident("diff") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("trait") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                path = lit.parse()?;
                Ok(())
            } else {
                Err(meta.error("unrecognized diff attribute, expected `trait = \"...\"`"))
            }
        })?;
    }

    Ok(path)
}

fn field_mode(field: &syn::Field) -> syn::Result<FieldMode> {
    let mut mode = FieldMode::Leaf;

    for attr in &field.attrs {
        if !attr.path().is_ident("diff") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("nested") {
                mode = FieldMode::Nested;
                Ok(())
            } else if meta.path.is_ident("always") {
                mode = FieldMode::Always;
                Ok(())
            } else if meta.path.is_ident("skip") {
                mode = FieldMode::Skip;
                Ok(())
            } else if meta.path.is_ident("leaf") {
                mode = FieldMode::Leaf;
                Ok(())
            } else {
                Err(meta.error(
                    "unrecognized diff attribute, expected `leaf`, `always`, `nested`, or `skip`",
                ))
            }
        })?;
    }

    Ok(mode)
}
