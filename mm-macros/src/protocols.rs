//! `protocols!` -- the request/response protocols carried over shared memory.
//!
//! ```ignore
//! mm_macros::protocols! {
//!     Handshake: (HandshakeRequest, u64),
//!     Tick: (GameState, FleetAction),
//! }
//! ```
//!
//! generates, for each `Name: (Request, Response)` pair, a `NameProtocol` marker type
//! implementing the `Protocol` trait, a pair of `Frame` variants, and an `on_name`
//! handler slot in `Handlers`.
//!
//! `Frame` is the type that is literally memcpy'd into the mapping, and the reader on
//! the other side identifies a frame by reading its first byte -- its tag -- and
//! comparing against `Protocol::request_tag()` / `response_tag()`, which are `ID * 2`
//! and `ID * 2 + 1`. That arithmetic is only correct if the variants are laid out
//! interleaved in declaration order, so the tags are written out explicitly here
//! rather than left to the compiler: an edit that reorders or inserts a variant then
//! fails to compile instead of silently misreading shared memory.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Ident, Token, Type,
};

use crate::teams::snake_case;

/// One `Name: (Request, Response)` entry.
pub(crate) struct ProtocolDef {
    name: Ident,
    request: Type,
    response: Type,
}

impl Parse for ProtocolDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let inner;
        syn::parenthesized!(inner in input);
        let request: Type = inner.parse()?;
        inner.parse::<Token![,]>()?;
        let response: Type = inner.parse()?;
        if !inner.is_empty() {
            return Err(inner.error("expected exactly a request and a response type"));
        }
        Ok(Self {
            name,
            request,
            response,
        })
    }
}

pub(crate) struct Protocols(Vec<ProtocolDef>);

impl Parse for Protocols {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let defs = Punctuated::<ProtocolDef, Token![,]>::parse_terminated(input)?;
        if defs.is_empty() {
            return Err(input.error("expected at least one protocol"));
        }
        Ok(Self(defs.into_iter().collect()))
    }
}

pub(crate) fn expand(protocols: Protocols) -> syn::Result<TokenStream> {
    let defs = &protocols.0;

    let ids = defs.iter().map(|d| &d.name);

    let impls = defs.iter().map(|d| {
        let name = &d.name;
        let marker = format_ident!("{}Protocol", name);
        let request_variant = format_ident!("{}Request", name);
        let response_variant = format_ident!("{}Response", name);
        let request = &d.request;
        let response = &d.response;
        quote! {
            pub struct #marker;

            impl Protocol for #marker {
                const ID: ProtocolId = ProtocolId::#name;
                type Request = #request;
                type Response = #response;

                fn request_into_frame(request: Self::Request) -> Frame {
                    Frame::#request_variant(request)
                }
                fn response_into_frame(response: Self::Response) -> Frame {
                    Frame::#response_variant(response)
                }
                fn frame_into_request(frame: Frame) -> Self::Request {
                    match frame {
                        Frame::#request_variant(ret) => ret,
                        _ => panic!(
                            concat!("expected a ", stringify!(#name), " request"),
                        ),
                    }
                }
                fn frame_into_response(frame: Frame) -> Self::Response {
                    match frame {
                        Frame::#response_variant(ret) => ret,
                        _ => panic!(
                            concat!("expected a ", stringify!(#name), " response"),
                        ),
                    }
                }
            }
        }
    });

    let variants = defs.iter().enumerate().map(|(i, d)| {
        let request_variant = format_ident!("{}Request", d.name);
        let response_variant = format_ident!("{}Response", d.name);
        let request = &d.request;
        let response = &d.response;
        // Must agree with `request_tag` / `response_tag` below.
        let request_tag = (i * 2) as u8;
        let response_tag = (i * 2 + 1) as u8;
        quote! {
            #request_variant(#request) = #request_tag,
            #response_variant(#response) = #response_tag,
        }
    });

    let handler_fields = defs.iter().map(|d| {
        let handler = format_ident!("on_{}", snake_case(&d.name.to_string()));
        let request = &d.request;
        let response = &d.response;
        quote! {
            pub #handler: ::std::boxed::Box<dyn Fn(&#request) -> #response>,
        }
    });

    let handler_arms = defs.iter().map(|d| {
        let handler = format_ident!("on_{}", snake_case(&d.name.to_string()));
        let request_variant = format_ident!("{}Request", d.name);
        let response_variant = format_ident!("{}Response", d.name);
        quote! {
            Frame::#request_variant(request) => {
                Frame::#response_variant((self.#handler)(request))
            }
        }
    });

    let response_arms = defs.iter().map(|d| {
        let response_variant = format_ident!("{}Response", d.name);
        quote! {
            Frame::#response_variant(_) => panic!(concat!(
                "engine sent a ",
                stringify!(#response_variant),
                ", which is a response and not a request",
            )),
        }
    });

    // `defs.len()` protocols means tags `0 ..= 2 * len - 1`; anything wider than a `u8`
    // would not fit the single byte the reader compares against.
    let count = defs.len();

    Ok(quote! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum ProtocolId {
            #(#ids,)*
        }

        pub trait Protocol {
            const ID: ProtocolId;
            type Request;
            type Response;
            fn request_into_frame(request: Self::Request) -> Frame;
            fn response_into_frame(response: Self::Response) -> Frame;
            fn frame_into_request(frame: Frame) -> Self::Request;
            fn frame_into_response(frame: Frame) -> Self::Response;
            fn request_tag() -> u8 {
                Self::ID as u8 * 2
            }
            fn response_tag() -> u8 {
                Self::ID as u8 * 2 + 1
            }
        }

        #(#impls)*

        #[derive(Clone)]
        #[repr(u8, C)]
        pub enum Frame {
            #(#variants)*
        }

        const _: () = assert!(
            #count * 2 <= u8::MAX as usize + 1,
            "too many protocols: the tag must fit in the single leading byte",
        );

        pub struct Handlers {
            #(#handler_fields)*
        }

        impl Handlers {
            pub fn respond(&self, frame: &Frame) -> Frame {
                match frame {
                    #(#handler_arms)*
                    #(#response_arms)*
                }
            }
        }
    })
}
