//! `#[teams(A, B)]` -- one item body, two naming schemes.
//!
//! The engine names the two sides absolutely (`A`/`B`) while a bot names them
//! relative to itself (`Me`/`Other`). The files that define `Team`, `TeamPair` and
//! `GameState` are shared verbatim between the two crates, so both namings have to
//! come out of one source. This attribute takes a module, rewrites the placeholder
//! identifiers in its body, and emits the items *without* the module wrapper, so
//! nothing about the surrounding paths changes:
//!
//! ```ignore
//! #[cfg_attr(feature = "engine", mm_macros::teams(A, B))]
//! #[cfg_attr(feature = "client", mm_macros::teams(Me, Other))]
//! mod _teams {
//!     pub enum Team { TeamA = 0, TeamB = 1 }
//!     pub struct TeamPair<T> { pub team_a: T, pub team_b: T }
//! }
//! ```
//!
//! Two placeholders, in two cases:
//!
//! | placeholder | `teams(A, B)` | `teams(Me, Other)` |
//! |---|---|---|
//! | `TeamA` / `TeamB` | `A` / `B` | `Me` / `Other` |
//! | `team_a` / `team_b` | `a` / `b` | `me` / `other` |
//!
//! Replacement is by *substring* of an identifier, not by whole token, so
//! `fleet_team_a` becomes `fleet_a` or `fleet_me`. That is the one thing this is for
//! that plain token substitution cannot do, and the reason the previous version of
//! this code needed `pastey::paste!`. Only `Ident` tokens are touched -- never string
//! literals, never lifetimes -- and `Team`/`TeamPair` contain neither placeholder, so
//! they pass through untouched.

use proc_macro2::{Group, Ident, TokenStream, TokenTree};
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    Error, ItemMod, Token,
};

/// The two team names supplied to the attribute, e.g. `A, B` or `Me, Other`.
pub(crate) struct TeamNames {
    a: Ident,
    b: Ident,
}

impl Parse for TeamNames {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let a: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let b: Ident = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("expected exactly two team names, e.g. `teams(A, B)`"));
        }
        if a == b {
            return Err(Error::new_spanned(&b, "the two team names must differ"));
        }
        Ok(Self { a, b })
    }
}

pub(crate) fn expand(names: TeamNames, module: ItemMod) -> syn::Result<TokenStream> {
    let items = match module.content {
        Some((_, ref items)) => items,
        None => {
            return Err(Error::new_spanned(
                &module,
                "#[teams] needs a module with a body, not a `mod foo;` declaration",
            ))
        }
    };

    // The placeholders cannot overlap each other inside a single identifier, so the
    // four substitutions are independent and order does not matter.
    let subs = [
        (String::from("TeamA"), names.a.to_string()),
        (String::from("TeamB"), names.b.to_string()),
        (String::from("team_a"), snake_case(&names.a.to_string())),
        (String::from("team_b"), snake_case(&names.b.to_string())),
    ];

    let body = quote! { #(#items)* };
    Ok(rewrite(body, &subs))
}

fn rewrite(tokens: TokenStream, subs: &[(String, String); 4]) -> TokenStream {
    tokens
        .into_iter()
        .map(|tt| match tt {
            // Descending into groups covers `#[...]` attributes for free -- they are
            // just bracketed token groups at this stage.
            TokenTree::Group(g) => {
                let mut new = Group::new(g.delimiter(), rewrite(g.stream(), subs));
                new.set_span(g.span());
                TokenTree::Group(new)
            }
            TokenTree::Ident(id) => {
                let mut name = id.to_string();
                let mut changed = false;
                for (from, to) in subs {
                    if name.contains(from.as_str()) {
                        name = name.replace(from.as_str(), to);
                        changed = true;
                    }
                }
                if changed {
                    TokenTree::Ident(Ident::new(&name, id.span()))
                } else {
                    TokenTree::Ident(id)
                }
            }
            other => other,
        })
        .collect()
}

/// `Other` -> `other`, `GameStart` -> `game_start`. `pastey`'s `:lower` gets the
/// second case wrong, which is half the reason for this crate.
pub(crate) fn snake_case(pascal: &str) -> String {
    let mut out = String::with_capacity(pascal.len() + 4);
    for (i, ch) in pascal.char_indices() {
        if ch.is_uppercase() && i != 0 {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}
