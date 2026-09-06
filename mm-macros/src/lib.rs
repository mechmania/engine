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

mod diff;
mod protocols;
mod teams;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput, Error, ItemMod};

/// Derives `Diff`. See the [`diff`] module docs for the field attributes.
#[proc_macro_derive(Diff, attributes(diff))]
pub fn derive_diff(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    diff::expand(input)
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
