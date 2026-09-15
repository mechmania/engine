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
//! implementing the `Protocol` trait and a pair of `Frame` variants.
//!
//! It deliberately generates no dispatch layer. There used to be a `Handlers` struct of
//! boxed closures, one slot per protocol, but both sides now read and write a payload at
//! `Frame::PAYLOAD_OFFSET` directly (`ipc::EngineChannel::request`, and `BotChannel`'s
//! `handshake`/`await_tick`/`respond`) -- a closure table in between bought nothing and
//! could not be split across an FFI boundary with Python in the middle.
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

    // Every payload type, in tag order, for `PAYLOAD_OFFSET` and `size_for_tag`.
    let payload_align_tys = defs
        .iter()
        .flat_map(|d| [d.request.clone(), d.response.clone()])
        .collect::<Vec<_>>();

    let size_arms = defs.iter().enumerate().flat_map(|(i, d)| {
        let request_tag = (i * 2) as u8;
        let response_tag = (i * 2 + 1) as u8;
        let request = d.request.clone();
        let response = d.response.clone();
        [
            quote! { #request_tag => Self::PAYLOAD_OFFSET + ::core::mem::size_of::<#request>(), },
            quote! { #response_tag => Self::PAYLOAD_OFFSET + ::core::mem::size_of::<#response>(), },
        ]
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
            // Both sides copy a payload in and out of the mapping by value, so the payload
            // types carry the bound rather than every call site repeating it.
            type Request: Clone + crate::game::mirror::LayoutHash;
            // The response is the direction that crosses *into* the engine from a process
            // it does not control, so it carries the validator as well.
            type Response: Clone + crate::game::mirror::LayoutHash + crate::game::mirror::Validate;
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

        impl Frame {
            /// Byte offset of a variant's payload within a `Frame`.
            ///
            /// Uniform across variants: `#[repr(u8, C)]` lays the enum out as a `#[repr(C)]`
            /// struct of the tag byte followed by a union of the variants' fields, so every
            /// payload starts at the same place -- the tag padded up to the greatest
            /// alignment any payload requires.
            pub const PAYLOAD_OFFSET: usize = {
                let mut align = 1usize;
                #(
                    if ::core::mem::align_of::<#payload_align_tys>() > align {
                        align = ::core::mem::align_of::<#payload_align_tys>();
                    }
                )*
                align
            };

            /// A hash of the measured layout of every payload that crosses this channel,
            /// in tag order -- the whole wire closure, since each payload type's own hash
            /// folds in its fields' types recursively.
            ///
            /// This is what makes a version skew loud. `mm-cli` pins one `mm-engine` git
            /// rev and a starterpack pins another; two builds that disagree about a struct
            /// disagree here, and the handshake says so instead of reading garbage for a
            /// whole match. Deriving it from the protocol list rather than a hand-written
            /// set of types means a new protocol extends it automatically.
            pub const LAYOUT_HASH: u64 = {
                let mut hash = crate::game::mirror::HASH_BASIS;
                #(
                    hash = crate::game::mirror::mix(
                        hash,
                        <#payload_align_tys as crate::game::mirror::LayoutHash>::HASH,
                    );
                )*
                hash
            };

            /// How many bytes the frame carrying `tag` actually occupies. `size_of::<Frame>()`
            /// is the *largest* variant, so copying that for every message would make the
            /// cheapest protocol pay for the dearest one.
            pub const fn size_for_tag(tag: u8) -> usize {
                match tag {
                    #(#size_arms)*
                    _ => ::core::mem::size_of::<Self>(),
                }
            }
        }

    })
}
