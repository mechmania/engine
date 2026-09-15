//! Proc macros shared by the engine and the bot libraries.
//!
//! Three of the files that define the game -- `ipc.rs`, `state.rs`, `team.rs` -- are
//! compiled by both the engine and every Rust bot, from the same bytes. They used to
//! rely on `macro_rules!` plus `pastey::paste!` to bridge the two builds; that hid
//! every item inside a macro body, cost a level of indentation, and got identifier
//! casing wrong for anything but single-word names. These macros replace that.
//!
//! - [`macro@Diff`] -- derive a sparse-JSON diff between two values (engine only).
//! - [`macro@teams`] -- one item body, two team naming schemes (see [`teams`]).
//! - [`protocols!`] -- the shared-memory request/response protocols.
//! - [`macro@mm_ffi_fn`] -- wrap a fn in a C-ABI entry point (see [`ffi_fn`]).
//! - [`macro@mm_ffi_handle`] -- mark a type as an opaque FFI handle (see [`ffi_handle`]).
//! - [`macro@FfiMirror`] -- record a type's wire layout (see [`ffi_mirror`]).

mod diff;
mod ffi_fn;
mod ffi_handle;
mod ffi_mirror;
mod protocols;
mod teams;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput, Error, ItemFn, ItemMod, ItemStruct};

/// Derives `Diff`. See the [`diff`] module docs for the field attributes.
#[proc_macro_derive(Diff, attributes(diff))]
pub fn derive_diff(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    diff::expand(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Derives `FfiMirrorType`. See the [`ffi_mirror`] module docs.
#[proc_macro_derive(FfiMirror)]
pub fn derive_ffi_mirror(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    ffi_mirror::expand(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Rewrites the team-name placeholders in a module body and emits its items inline.
/// See the [`teams`] module docs.
#[proc_macro_attribute]
pub fn teams(args: TokenStream, item: TokenStream) -> TokenStream {
    let names = parse_macro_input!(args as teams::TeamNames);
    let module = parse_macro_input!(item as ItemMod);
    teams::expand(names, module)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Defines the shared-memory protocols. See the [`protocols`] module docs.
#[proc_macro]
pub fn protocols(input: TokenStream) -> TokenStream {
    let protocols = parse_macro_input!(input as protocols::Protocols);
    protocols::expand(protocols)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Generates a `#[no_mangle] extern "C"` wrapper, with `catch_unwind` and a panic
/// sentinel, around a plain Rust fn. See the [`ffi_fn`] module docs.
#[proc_macro_attribute]
pub fn mm_ffi_fn(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as ffi_fn::Args);
    let item = parse_macro_input!(item as ItemFn);
    ffi_fn::expand(args, item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Marks a type as an opaque handle Python holds across calls, generating its
/// `_free` entry point. See the [`ffi_handle`] module docs.
#[proc_macro_attribute]
pub fn mm_ffi_handle(args: TokenStream, item: TokenStream) -> TokenStream {
    if !args.is_empty() {
        let args: proc_macro2::TokenStream = args.into();
        return Error::new_spanned(args, "`#[mm_ffi_handle]` takes no arguments")
            .into_compile_error()
            .into();
    }
    let item = parse_macro_input!(item as ItemStruct);
    ffi_handle::expand(item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}
