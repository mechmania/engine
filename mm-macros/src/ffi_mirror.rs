//! Derive recording a type's wire layout for the binding generator, the action validator
//! and the handshake fingerprint.
//!
//! ```ignore
//! #[derive(FfiMirror)]
//! #[repr(C)]
//! pub struct Vec2 { pub x: f32, pub y: f32 }
//! ```
//!
//! emits `impl FfiMirrorType for Vec2 { const DESC: FfiMirrorDesc = ...; }` carrying each
//! field's `offset_of!`, `size_of` and `align_of`, plus the type *as it was written* --
//! `BotId`, `Map`, `[BotAction; BOTS_MAX]`. A derive sees tokens, not types: it cannot
//! resolve an alias, a length constant, or anything else that needs name resolution, so it
//! records the spelling and lets the generator resolve it against the registry. It
//! changes nothing about layout or runtime behaviour.
//!
//! Two more impls come out of the same analysis, and are emitted for every mirrored type:
//!
//! - `impl Validate` -- is a byte image a valid value? Every declared tag, every `bool`.
//!   The engine calls it on the raw mapping before `EngineChannel::request` materializes a
//!   `FleetAction` a foreign-language bot wrote.
//! - `impl LayoutHash` -- a `const` hash of the measured layout, which the handshake sends
//!   so that two builds that disagree about a struct say so at tick 0.
//!
//! Both recurse through the *real field types* (`<FieldTy as Validate>::validate(..)`)
//! rather than walking the descriptors below: a descriptor spells a field's type as a
//! string, and resolving that name needs the `inventory` registry, which only an `ffi`
//! build has -- while both of these run engine-side. The derive has the field's `syn::Type`
//! tokens, so it gets the resolution for free. A generic impl picks up the matching bound
//! on its parameters; nothing else changes.
//!
//! Unlike [`crate::ffi_fn`] and [`crate::ffi_handle`], all three impls are emitted
//! unconditionally -- `crate::game::mirror` is shared source compiled in every build, and
//! `ffi` implies `client`. Only the `inventory::submit!` that puts a descriptor in the
//! registry is `#[cfg(feature = "ffi")]`.
//!
//! **Data-carrying enums.** `offset_of!` cannot reach into an enum variant on stable, so
//! the derive builds a shadow of the layout the Reference defines for `#[repr(u8, C)]` --
//! a `#[repr(C)]` struct of `{ tag: u8, payload: union of per-variant #[repr(C)] structs }`
//! -- and measures that. The union matters: it carries the max-alignment padding, which a
//! per-variant shadow would under-report whenever one variant is less aligned than another.
//! The model is not taken on faith; `DESC` asserts the shadow's size and alignment equal
//! the real enum's, so a layout change is a compile error rather than a wrong offset in
//! generated Python.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{Data, DeriveInput, Error, Fields, GenericParam, Ident, Meta, Variant};

/// The path to the shared metadata module. Absolute, and spelled `crate::game::` because
/// that is the name both the engine and every bot crate reach these files by -- the bot
/// crates re-export `crate::core` as a fake `game` module for exactly this reason.
fn mirror() -> TokenStream {
    quote!(crate::game::mirror)
}

pub(crate) fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let ident = &input.ident;
    let repr = Repr::parse(&input)?;

    for param in &input.generics.params {
        match param {
            GenericParam::Type(_) => {}
            other => {
                return Err(Error::new_spanned(
                    other,
                    "FfiMirror supports type parameters only -- a lifetime or const \
                     parameter has no place in a `#[repr(C)]` wire type",
                ))
            }
        }
    }
    let generic_params: Vec<String> = input
        .generics
        .type_params()
        .map(|p| p.ident.to_string())
        .collect();
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let self_ty = quote!(#ident #ty_generics);

    let m = mirror();
    let mut shadows = TokenStream::new();

    let (kind, size_align_assert, validate_body, hash_expr) = match &input.data {
        Data::Struct(data) => {
            if repr != Repr::C {
                return Err(Error::new_spanned(
                    ident,
                    "FfiMirror requires `#[repr(C)]` on a struct -- the wire layout must \
                     not be Rust's to choose",
                ));
            }
            let fields = named_fields(&data.fields, ident)?;
            let descs = fields
                .iter()
                .map(|f| {
                    let name = f.name.clone();
                    let ty = &f.ty;
                    let ty_str = spell(ty);
                    let access = &f.access;
                    quote! {
                        #m::FfiFieldDesc {
                            name: #name,
                            ty: #ty_str,
                            offset: ::core::mem::offset_of!(#self_ty, #access),
                            size: ::core::mem::size_of::<#ty>(),
                            align: ::core::mem::align_of::<#ty>(),
                        }
                    }
                })
                .collect::<Vec<_>>();
            let checks = fields.iter().map(|f| {
                let ty = &f.ty;
                let access = &f.access;
                quote! {
                    && <#ty as #m::Validate>::validate(
                        &bytes[::core::mem::offset_of!(#self_ty, #access)..]
                    )
                }
            });
            let mut hash = hash_seed(&m, 0, &self_ty);
            for f in &fields {
                let ty = &f.ty;
                let access = &f.access;
                hash = quote! {
                    #m::mix(
                        #m::mix(#hash, ::core::mem::offset_of!(#self_ty, #access) as u64),
                        <#ty as #m::LayoutHash>::HASH,
                    )
                };
            }
            (
                quote!(#m::FfiKind::Struct { fields: &[#(#descs),*] }),
                TokenStream::new(),
                quote!(true #(#checks)*),
                hash,
            )
        }
        Data::Enum(data) => {
            if data.variants.is_empty() {
                return Err(Error::new_spanned(
                    ident,
                    "FfiMirror cannot describe an enum with no variants",
                ));
            }
            let tags = tags(&data.variants)?;
            let has_payload = data
                .variants
                .iter()
                .any(|v| !matches!(v.fields, Fields::Unit));

            if !has_payload {
                if repr != Repr::U8 {
                    return Err(Error::new_spanned(
                        ident,
                        "FfiMirror requires `#[repr(u8)]` on a fieldless enum -- a C-int \
                         tag is four bytes and disagrees with every other tag on the wire",
                    ));
                }
                let descs = data.variants.iter().map(|v| {
                    let vname = &v.ident;
                    let vname_str = vname.to_string();
                    // Measured rather than counted: the cast is const for a fieldless
                    // enum, so an explicit discriminant cannot be got wrong here.
                    quote! {
                        #m::FfiVariantDesc {
                            name: #vname_str,
                            tag: #self_ty::#vname as u8,
                            fields: &[],
                        }
                    }
                });
                let mut hash = hash_seed(&m, 1, &self_ty);
                for &tag in &tags {
                    let tag = tag as u64;
                    hash = quote!(#m::mix(#hash, #tag));
                }
                let known = tags.iter();
                (
                    quote!(#m::FfiKind::UnitEnum { variants: &[#(#descs),*] }),
                    TokenStream::new(),
                    quote!(::core::matches!(bytes[0], #(#known)|*)),
                    hash,
                )
            } else {
                if repr != Repr::U8C {
                    return Err(Error::new_spanned(
                        ident,
                        "FfiMirror requires `#[repr(u8, C)]` on a data-carrying enum -- a \
                         plain `#[repr(C)]` enum has a four-byte tag, and two tag widths in \
                         one mirror is a bug generator for every port",
                    ));
                }
                let (kind, assert, emitted, validate, hash) =
                    data_enum(ident, &data.variants, &tags, &input.generics, &self_ty)?;
                shadows = emitted;
                (kind, assert, validate, hash)
            }
        }
        Data::Union(_) => {
            return Err(Error::new_spanned(
                ident,
                "FfiMirror cannot describe a union -- nothing on the wire is one",
            ))
        }
    };

    let name_str = ident.to_string();
    let param_strs = generic_params.iter();

    // A generic type registers nothing: `inventory::submit!` places its value in a static
    // initializer and cannot produce one per monomorphization, so the live instantiations
    // are enumerated by hand next to the collectors in `crate::ffi`. The path is spelled
    // `::inventory::` because `mm-macros`' manifest is hardlinked into both bot crates and
    // must not gain the dependency -- see `engine/Cargo.toml`.
    let registration = if generic_params.is_empty() {
        quote! {
            #[cfg(feature = "ffi")]
            ::inventory::submit! { <#ident as #m::FfiMirrorType>::DESC }
        }
    } else {
        TokenStream::new()
    };

    let validate_generics = bounded(&input.generics, &quote!(#m::Validate));
    let (validate_impl_generics, _, validate_where) = validate_generics.split_for_impl();
    let hash_generics = bounded(&input.generics, &quote!(#m::LayoutHash));
    let (hash_impl_generics, _, hash_where) = hash_generics.split_for_impl();

    Ok(quote! {
        #shadows

        impl #impl_generics #m::FfiMirrorType for #self_ty #where_clause {
            const DESC: #m::FfiMirrorDesc = {
                #size_align_assert
                #m::FfiMirrorDesc {
                    name: #name_str,
                    generic_params: &[#(#param_strs),*],
                    generic_args: &[],
                    size: ::core::mem::size_of::<#self_ty>(),
                    align: ::core::mem::align_of::<#self_ty>(),
                    kind: #kind,
                }
            };
        }

        impl #validate_impl_generics #m::Validate for #self_ty #validate_where {
            #[allow(unused_variables)]
            fn validate(bytes: &[u8]) -> bool {
                #validate_body
            }
        }

        impl #hash_impl_generics #m::LayoutHash for #self_ty #hash_where {
            const HASH: u64 = #hash_expr;
        }

        #registration
    })
}

/// The seed a type's layout hash starts from: what shape it is, and its measured size and
/// alignment. Field offsets, variant tags and each field type's own hash are mixed in on
/// top of this. No name ever is -- see [`crate::ffi_mirror`]'s module docs.
fn hash_seed(m: &TokenStream, kind: u64, self_ty: &TokenStream) -> TokenStream {
    quote! {
        #m::mix(
            #m::mix(#m::HASH_BASIS, #kind),
            (::core::mem::size_of::<#self_ty>() * 256
                + ::core::mem::align_of::<#self_ty>()) as u64,
        )
    }
}

/// `impl<T> Trait for Foo<T>` needs `T: Trait` for a recursive impl to hold. The descriptor
/// impl needs no such bound, so the two are built separately rather than bounding the
/// original generics in place.
fn bounded(generics: &syn::Generics, bound: &TokenStream) -> syn::Generics {
    let mut out = generics.clone();
    for param in out.type_params_mut() {
        param.bounds.push(syn::parse_quote!(#bound));
    }
    out
}

// -----------------------------------------------------------------------------------
// data-carrying enums: the shadow layout
// -----------------------------------------------------------------------------------

fn data_enum(
    ident: &Ident,
    variants: &syn::punctuated::Punctuated<Variant, syn::Token![,]>,
    tags: &[u8],
    generics: &syn::Generics,
    self_ty: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream, TokenStream, TokenStream, TokenStream)> {
    let m = mirror();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let phantom = if generics.type_params().next().is_some() {
        let params = generics.type_params().map(|p| &p.ident);
        // A variant that names none of the parameters would otherwise be an unused-param
        // error. `PhantomData` is a ZST of align 1, so appending it last changes no
        // offset and no alignment.
        Some(quote!(__ffi_mirror_phantom: ::core::marker::PhantomData<(#(#params,)*)>,))
    } else {
        None
    };

    let mut shadows = TokenStream::new();
    let mut union_fields = Vec::new();
    let mut variant_descs = Vec::new();
    let mut validate_arms = Vec::new();
    let mut hash = hash_seed(&m, 2, self_ty);
    let repr_ty = format_ident!("__FfiMirror_{}_Repr", ident);
    let payload_ty = format_ident!("__FfiMirror_{}_Payload", ident);

    hash = quote! {
        #m::mix(#hash, ::core::mem::offset_of!(#repr_ty #ty_generics, payload) as u64)
    };

    for (variant, &tag) in variants.iter().zip(tags) {
        let vname = &variant.ident;
        let vname_str = vname.to_string();
        let shadow_ty = format_ident!("__FfiMirror_{}_{}", ident, vname);
        let fields = named_fields(&variant.fields, ident)?;

        let decls = fields.iter().map(|f| {
            let id = &f.shadow;
            let ty = &f.ty;
            quote!(#id: #ty,)
        });
        shadows.extend(quote! {
            #[doc(hidden)]
            #[allow(non_camel_case_types, dead_code)]
            #[repr(C)]
            struct #shadow_ty #impl_generics #where_clause { #(#decls)* #phantom }
        });

        let field_descs = fields.iter().map(|f| {
            let name = f.name.clone();
            let id = &f.shadow;
            let ty = &f.ty;
            let ty_str = spell(ty);
            quote! {
                #m::FfiFieldDesc {
                    name: #name,
                    ty: #ty_str,
                    offset: ::core::mem::offset_of!(#repr_ty #ty_generics, payload)
                        + ::core::mem::offset_of!(#shadow_ty #ty_generics, #id),
                    size: ::core::mem::size_of::<#ty>(),
                    align: ::core::mem::align_of::<#ty>(),
                }
            }
        });
        variant_descs.push(quote! {
            #m::FfiVariantDesc {
                name: #vname_str,
                tag: #tag,
                fields: &[#(#field_descs),*],
            }
        });

        // The same offsets the descriptor records, reused rather than recomputed: a
        // variant's payload starts where the shadow says it does, and a validator reading
        // anywhere else would be checking padding.
        let offsets: Vec<TokenStream> = fields
            .iter()
            .map(|f| {
                let id = &f.shadow;
                quote! {
                    ::core::mem::offset_of!(#repr_ty #ty_generics, payload)
                        + ::core::mem::offset_of!(#shadow_ty #ty_generics, #id)
                }
            })
            .collect();
        let checks = fields.iter().zip(&offsets).map(|(f, offset)| {
            let ty = &f.ty;
            quote!(&& <#ty as #m::Validate>::validate(&bytes[#offset..]))
        });
        validate_arms.push(quote!(#tag => true #(#checks)*,));

        let tag_hash = tag as u64;
        hash = quote!(#m::mix(#hash, #tag_hash));
        for (f, offset) in fields.iter().zip(&offsets) {
            let ty = &f.ty;
            hash = quote! {
                #m::mix(
                    #m::mix(#hash, (#offset) as u64),
                    <#ty as #m::LayoutHash>::HASH,
                )
            };
        }

        let union_field = format_ident!("{}", crate::teams::snake_case(&vname_str));
        union_fields.push(quote!(#union_field: ::core::mem::ManuallyDrop<#shadow_ty #ty_generics>,));
    }

    shadows.extend(quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types, dead_code)]
        #[repr(C)]
        union #payload_ty #impl_generics #where_clause { #(#union_fields)* }

        #[doc(hidden)]
        #[allow(non_camel_case_types, dead_code)]
        #[repr(C)]
        struct #repr_ty #impl_generics #where_clause {
            tag: u8,
            payload: #payload_ty #ty_generics,
        }
    });

    let assert = quote! {
        // The shadow models what the Reference says `#[repr(u8, C)]` is. If that ever
        // stops being true, fail here rather than hand the generator a wrong offset.
        assert!(
            ::core::mem::size_of::<#repr_ty #ty_generics>() == ::core::mem::size_of::<#self_ty>(),
            "FfiMirror shadow layout disagrees with the real enum's size",
        );
        assert!(
            ::core::mem::align_of::<#repr_ty #ty_generics>() == ::core::mem::align_of::<#self_ty>(),
            "FfiMirror shadow layout disagrees with the real enum's alignment",
        );
    };

    let kind = quote! {
        #m::FfiKind::DataEnum {
            payload_offset: ::core::mem::offset_of!(#repr_ty #ty_generics, payload),
            variants: &[#(#variant_descs),*],
        }
    };
    // A tag the source never declared is the case this whole mechanism exists for: it is
    // what a hand-filled buffer produces, and what `std::ptr::read` would turn into an
    // invalid value in the *engine's* process.
    let validate = quote! {
        match bytes[0] {
            #(#validate_arms)*
            _ => false,
        }
    };

    Ok((kind, assert, shadows, validate, hash))
}

// -----------------------------------------------------------------------------------
// helpers
// -----------------------------------------------------------------------------------

struct FieldInfo {
    /// The name as the wire sees it: the field name, or the index for a tuple field.
    name: String,
    /// How to reach it on the real type -- `pos`, or `0` for a tuple field.
    access: TokenStream,
    /// What it is called on the shadow struct, where a numeric name is not allowed.
    shadow: Ident,
    ty: syn::Type,
}

fn named_fields(fields: &Fields, ident: &Ident) -> syn::Result<Vec<FieldInfo>> {
    let out = match fields {
        Fields::Named(named) => named
            .named
            .iter()
            .map(|f| {
                let name = f.ident.as_ref().unwrap();
                FieldInfo {
                    name: name.to_string(),
                    access: name.to_token_stream(),
                    shadow: name.clone(),
                    ty: f.ty.clone(),
                }
            })
            .collect(),
        Fields::Unnamed(unnamed) => unnamed
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let index = syn::Index::from(i);
                FieldInfo {
                    name: i.to_string(),
                    access: index.to_token_stream(),
                    shadow: format_ident!("f{}", i),
                    ty: f.ty.clone(),
                }
            })
            .collect(),
        Fields::Unit => Vec::new(),
    };
    if out.is_empty() && matches!(fields, Fields::Unnamed(_)) {
        return Err(Error::new_spanned(
            ident,
            "FfiMirror cannot describe an empty tuple variant",
        ));
    }
    Ok(out)
}

/// The tag each variant carries. Explicit discriminants are honoured; implicit ones
/// continue from the last, as Rust itself defines them.
fn tags(variants: &syn::punctuated::Punctuated<Variant, syn::Token![,]>) -> syn::Result<Vec<u8>> {
    let mut next: u32 = 0;
    let mut out = Vec::with_capacity(variants.len());
    for variant in variants {
        if let Some((_, expr)) = &variant.discriminant {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(int),
                ..
            }) = expr
            else {
                return Err(Error::new_spanned(
                    expr,
                    "FfiMirror needs an integer-literal discriminant -- it has to know the \
                     tag byte to record it, and it cannot evaluate a const expression",
                ));
            };
            next = int.base10_parse()?;
        }
        let tag = u8::try_from(next).map_err(|_| {
            Error::new_spanned(&variant.ident, "a tag byte cannot exceed 255")
        })?;
        out.push(tag);
        next += 1;
    }
    Ok(out)
}

/// The type as written, whitespace-normalized: `[BotAction; BOTS_MAX]`,
/// `StateOption<Vec2>`, `[[MapTile; MAP_SIZE]; MAP_SIZE]`.
fn spell(ty: &syn::Type) -> String {
    let raw = ty.to_token_stream().to_string();
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_whitespace() {
            continue;
        }
        out.push(ch);
        if ch == ';' || ch == ',' {
            out.push(' ');
        }
    }
    out
}

#[derive(PartialEq, Eq, Debug)]
enum Repr {
    C,
    U8,
    U8C,
    Other,
}

impl Repr {
    /// Reads the `#[repr(..)]` the type actually carries. The derive does not *set* the
    /// repr -- it refuses to describe a type whose layout Rust is still free to choose.
    fn parse(input: &DeriveInput) -> syn::Result<Repr> {
        let mut c = false;
        let mut u8_ = false;
        for attr in &input.attrs {
            if !attr.path().is_ident("repr") {
                continue;
            }
            let Meta::List(list) = &attr.meta else { continue };
            list.parse_nested_meta(|meta| {
                if meta.path.is_ident("C") {
                    c = true;
                } else if meta.path.is_ident("u8") {
                    u8_ = true;
                }
                // Anything else -- another primitive tag type, `packed`, `align(..)` --
                // is not our business; `(c, u8)` below decides. Deliberately no
                // `meta.input.parse()` here: that would swallow the rest of the list,
                // which is how `#[repr(u8, C)]` first read as a bare `u8`.
                Ok(())
            })?;
        }
        Ok(match (c, u8_) {
            (true, true) => Repr::U8C,
            (true, false) => Repr::C,
            (false, true) => Repr::U8,
            (false, false) => Repr::Other,
        })
    }
}
