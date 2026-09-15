//! Attribute macro generating a C-ABI wrapper around a plain Rust function.
//!
//! Every entry point Python reaches through `ctypes` has the same shape: an
//! `extern "C"` signature over ABI-safe scalars, a `catch_unwind` so a Rust panic
//! unwinding into C (undefined behaviour) becomes a sentinel return instead, and a
//! registration so the binding generator knows the function exists. That is pure
//! boilerplate, three times over per function, and the failure mode when someone
//! forgets the `catch_unwind` is a crash in a competitor's bot rather than a
//! compile error. This macro writes all three.
//!
//! ```ignore
//! #[mm_ffi_fn]
//! pub fn mm_path_length(ch: *const MmChannel, fx: f32, fy: f32, tx: f32, ty: f32) -> f32 {
//!     let ch = unsafe { &*ch };
//!     topology::path_length(&ch.config, Vec2::new(fx, fy), Vec2::new(tx, ty)).unwrap_or(-1.0)
//! }
//! ```
//!
//! becomes a private `fn mm_path_length_impl(..)` holding the body, plus a
//! `#[no_mangle] pub extern "C" fn mm_path_length(..)` with the identical signature
//! that calls it inside `catch_unwind` and returns `f32::NAN` if it caught.
//!
//! # Sentinels
//!
//! The default panic sentinel comes from the return type: `f32::NAN` for `f32`,
//! `false` for `bool`, `-1` for signed integers, null for raw pointers, and nothing
//! at all for `()`. Override it when the default value already means something else
//! to the caller:
//!
//! ```ignore
//! #[mm_ffi_fn(panic = -2)]
//! pub fn mm_route_waypoints(..) -> i32 { .. }   // -1 already means "no route"
//! ```
//!
//! # Argument types
//!
//! Restricted to `f32`, `u32`, `i32`, `u8`, `bool` and raw pointers. Notably absent
//! is passing a small struct like `Vec2` by value: that is the corner of the C ABI
//! where `ctypes`, System V and MSVC are most likely to disagree, and unpacking to
//! loose floats costs nothing. Anything else is a spanned compile error here rather
//! than garbage at the `ctypes` boundary.
//!
//! # Scope
//!
//! The generated `inventory::submit!` names `crate::ffi::FfiFnDesc`, so this macro
//! only works inside `mm-engine`'s `ffi` module. It is not a general-purpose export
//! attribute and is not meant to be one.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Error, Expr, FnArg, ItemFn, Pat, ReturnType, Type};

/// The panic sentinel: either inferred from the return type or given explicitly.
pub(crate) struct Args {
    panic: Option<Expr>,
}

impl syn::parse::Parse for Args {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Args { panic: None });
        }
        let key: syn::Ident = input.parse()?;
        if key != "panic" {
            return Err(Error::new_spanned(
                &key,
                "unrecognized argument, expected `panic = <expr>`",
            ));
        }
        input.parse::<syn::Token![=]>()?;
        let panic = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("expected only `panic = <expr>`"));
        }
        Ok(Args { panic: Some(panic) })
    }
}

/// The ABI-safe type set, and how each maps onto a `crate::ffi::FfiType` variant and
/// a default panic sentinel.
enum AbiType {
    Void,
    F32,
    U32,
    I32,
    U8,
    Bool,
    /// A raw pointer, carrying the pointee's name for the generated descriptor.
    Ptr { pointee: String, mutable: bool },
}

impl AbiType {
    fn parse(ty: &Type) -> syn::Result<Self> {
        match ty {
            Type::Path(p) if p.qself.is_none() => {
                let ident = p.path.get_ident().ok_or_else(|| {
                    Error::new_spanned(ty, "not an ABI-safe type: expected a bare type name")
                })?;
                match ident.to_string().as_str() {
                    "f32" => Ok(AbiType::F32),
                    "u32" => Ok(AbiType::U32),
                    "i32" => Ok(AbiType::I32),
                    "u8" => Ok(AbiType::U8),
                    "bool" => Ok(AbiType::Bool),
                    other => Err(Error::new_spanned(
                        ty,
                        format!(
                            "`{other}` is not ABI-safe across the ctypes boundary; \
                             expected `f32`, `u32`, `i32`, `u8`, `bool`, or a raw pointer. \
                             Pass a `Vec2` as loose `f32`s, not by value."
                        ),
                    )),
                }
            }
            Type::Ptr(p) => {
                let pointee = match &*p.elem {
                    Type::Path(inner) => inner
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_else(|| "void".into()),
                    _ => "void".into(),
                };
                Ok(AbiType::Ptr { pointee, mutable: p.mutability.is_some() })
            }
            _ => Err(Error::new_spanned(
                ty,
                "not an ABI-safe type; expected `f32`, `u32`, `i32`, `u8`, `bool`, \
                 or a raw pointer",
            )),
        }
    }

    /// The `crate::ffi::FfiType` value describing this type to the binding generator.
    fn descriptor(&self) -> TokenStream {
        match self {
            AbiType::Void => quote!(crate::ffi::FfiType::Void),
            AbiType::F32 => quote!(crate::ffi::FfiType::F32),
            AbiType::U32 => quote!(crate::ffi::FfiType::U32),
            AbiType::I32 => quote!(crate::ffi::FfiType::I32),
            AbiType::U8 => quote!(crate::ffi::FfiType::U8),
            AbiType::Bool => quote!(crate::ffi::FfiType::Bool),
            AbiType::Ptr { pointee, mutable } => {
                quote!(crate::ffi::FfiType::Ptr { pointee: #pointee, mutable: #mutable })
            }
        }
    }

    /// What the `extern "C"` wrapper returns when `catch_unwind` caught a panic.
    fn default_sentinel(&self, span: proc_macro2::Span) -> syn::Result<TokenStream> {
        match self {
            AbiType::Void => Ok(quote!(())),
            AbiType::F32 => Ok(quote!(f32::NAN)),
            AbiType::I32 => Ok(quote!(-1i32)),
            AbiType::Bool => Ok(quote!(false)),
            AbiType::Ptr { mutable, .. } => {
                Ok(if *mutable { quote!(::std::ptr::null_mut()) } else { quote!(::std::ptr::null()) })
            }
            // Every value of these is a legitimate answer, so there is nothing to
            // reserve. Demand an explicit choice rather than inventing one.
            AbiType::U32 | AbiType::U8 => Err(Error::new(
                span,
                "no default panic sentinel for an unsigned return type -- every value \
                 is a valid result. Give one explicitly: `#[mm_ffi_fn(panic = ...)]`",
            )),
        }
    }
}

pub(crate) fn expand(args: Args, item: ItemFn) -> syn::Result<TokenStream> {
    if let Some(abi) = &item.sig.abi {
        return Err(Error::new_spanned(
            abi,
            "write a plain Rust fn; `#[mm_ffi_fn]` generates the `extern \"C\"` wrapper",
        ));
    }
    if item.sig.asyncness.is_some() {
        return Err(Error::new_spanned(
            &item.sig,
            "an `async fn` cannot cross the C ABI; the C surface is blocking by construction",
        ));
    }
    if !item.sig.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &item.sig.generics,
            "a generic fn has no single symbol to export",
        ));
    }

    let name = &item.sig.ident;
    let vis = &item.vis;
    let unsafety = &item.sig.unsafety;

    // Argument names and types, validated against the ABI-safe set. The wrapper
    // re-declares them identically and forwards them untouched.
    let mut arg_names = Vec::new();
    let mut arg_decls = Vec::new();
    let mut arg_descs = Vec::new();
    for arg in &item.sig.inputs {
        let FnArg::Typed(pat_ty) = arg else {
            return Err(Error::new_spanned(
                arg,
                "`#[mm_ffi_fn]` applies to free functions, not methods -- there is no \
                 `self` on the other side of the C ABI",
            ));
        };
        let Pat::Ident(pat_ident) = &*pat_ty.pat else {
            return Err(Error::new_spanned(
                &pat_ty.pat,
                "expected a plain argument name; the wrapper has to re-declare it",
            ));
        };
        let ident = &pat_ident.ident;
        let ty = &pat_ty.ty;
        let abi = AbiType::parse(ty)?;
        let desc = abi.descriptor();
        let name_str = ident.to_string();

        arg_names.push(ident.clone());
        arg_decls.push(quote!(#ident: #ty));
        arg_descs.push(quote!((#name_str, #desc)));
    }

    let (ret_ty, ret_abi) = match &item.sig.output {
        ReturnType::Default => (quote!(), AbiType::Void),
        ReturnType::Type(_, ty) => (quote!(-> #ty), AbiType::parse(ty)?),
    };
    let ret_desc = ret_abi.descriptor();
    let sentinel = match args.panic {
        Some(expr) => quote!(#expr),
        None => ret_abi.default_sentinel(item.sig.ident.span())?,
    };

    let inner = format_ident!("{}_impl", name);
    let name_str = name.to_string();

    // The body moves to `#inner` verbatim; only the identifier changes, so a
    // rustc error inside it still points at the code as written.
    let mut inner_fn = item.clone();
    inner_fn.sig.ident = inner.clone();
    inner_fn.vis = syn::Visibility::Inherited;

    // `AssertUnwindSafe` because the arguments are raw pointers and scalars: there is
    // no `&mut` whose invariants a panic could leave broken, and the handle behind the
    // pointer is immutable for the life of the match.
    let call = if unsafety.is_some() {
        quote!(unsafe { #inner(#(#arg_names),*) })
    } else {
        quote!(#inner(#(#arg_names),*))
    };

    Ok(quote! {
        #inner_fn

        #[no_mangle]
        #vis extern "C" fn #name(#(#arg_decls),*) #ret_ty {
            match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| #call)) {
                Ok(value) => value,
                Err(_) => #sentinel,
            }
        }

        ::inventory::submit! {
            crate::ffi::FfiFnDesc {
                name: #name_str,
                ret: #ret_desc,
                args: &[#(#arg_descs),*],
            }
        }
    })
}
