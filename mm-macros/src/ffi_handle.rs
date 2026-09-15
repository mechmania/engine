//! Attribute macro marking a type as an opaque FFI handle.
//!
//! Python cannot see inside a Rust struct, so anything it has to hold across calls
//! -- the `GameConfig`, later the mmap'd channel and its tokio runtime -- crosses as
//! a bare pointer that Python stores as a `ctypes.c_void_p` and hands back on every
//! call. "Opaque" is the whole point: the layout is Rust's business, and Python
//! never depends on it.
//!
//! ```ignore
//! #[mm_ffi_handle]
//! pub struct MmChannel { config: GameConfig }
//! ```
//!
//! generates `mm_channel_free`, which reclaims the `Box` (and tolerates null, since
//! a Python `__del__` racing a failed constructor is exactly when this gets called
//! with one), plus the registration the binding generator reads to emit the matching
//! Python wrapper class.
//!
//! Constructors are *not* generated. Every handle is built differently -- opening a
//! file, running a handshake -- so that stays a hand-written `#[mm_ffi_fn]`
//! returning `*mut Self`. Only the freeing is uniform.
//!
//! Like [`crate::ffi_fn`], the generated `inventory::submit!` names
//! `crate::ffi::FfiHandleDesc` and so only works inside `mm-engine`'s `ffi` module.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Error, ItemStruct};

pub(crate) fn expand(item: ItemStruct) -> syn::Result<TokenStream> {
    if !item.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &item.generics,
            "a generic type has no single `_free` symbol to export",
        ));
    }

    let name = &item.ident;
    let name_str = name.to_string();
    // `MmChannel` -> `mm_channel_free`. The type is named for the C symbol it becomes,
    // so the prefix is already in the name -- don't add a second `mm_`.
    let free = format_ident!("{}_free", crate::teams::snake_case(&name_str));
    let free_str = free.to_string();

    Ok(quote! {
        #item

        /// Reclaims a handle allocated by its constructor. Null is a no-op, and
        /// double-freeing is the caller's bug -- as in any C API.
        #[no_mangle]
        pub extern "C" fn #free(ptr: *mut #name) {
            if ptr.is_null() {
                return;
            }
            // Nothing here can unwind: `Box::from_raw` does not, and the drop glue for
            // a handle is its fields' drop glue. No `catch_unwind` wrapper needed.
            drop(unsafe { ::std::boxed::Box::from_raw(ptr) });
        }

        ::inventory::submit! {
            crate::ffi::FfiHandleDesc { name: #name_str, free: #free_str }
        }
    })
}
