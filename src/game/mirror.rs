//! Layout metadata for the types that cross the engine <-> bot boundary.
//!
//! `#[derive(FfiMirror)]` records, per type, the facts a *reader of the raw bytes* needs:
//! size, alignment, every field's offset and the type it was written as, and for an enum
//! every variant's tag. Three consumers read it:
//!
//! - the Python binding generator (`mm-ffi-codegen`), which turns descriptors into
//!   `ctypes.Structure`s -- see `dev/ffi.md`;
//! - [`Validate`], which rejects a malformed `FleetAction` before the engine materializes it;
//! - [`LayoutHash`], the handshake's layout fingerprint, which hashes the measured facts
//!   (never the names -- `mm_macros::teams` legitimately renames fields between an `engine`
//!   and a `client` build, so a name-sensitive hash would reject every correct pair).
//!
//! The last two are *also* emitted by `#[derive(FfiMirror)]`, as impls that recurse through
//! the real field types, rather than as walks over the descriptors below. A descriptor
//! spells a field's type as a string, and resolving that name needs the `inventory` registry
//! -- which is `ffi`-only, while both of those run in an `engine` build.
//!
//! **This module is compiled in every build, not just `ffi`.** The registry that collects
//! descriptors (`inventory::collect!`) lives in `crate::ffi` and is `ffi`-only, but the
//! metadata itself has to exist in an `engine` build too, because the `Validate` check runs
//! engine-side. `#[derive(FfiMirror)]` is correspondingly unconditional; only the
//! `inventory::submit!` it emits is gated.
//!
//! Note the deliberate absence of any `name` in the fingerprint's reach and the presence of
//! `ty` spelled *as written* (`BotId`, `Map`, `[BotAction; BOTS_MAX]`): a derive sees tokens,
//! not types, so it cannot resolve an alias or a length constant. The generator resolves
//! names against this registry instead, and recovers an array's length by dividing the
//! field's measured `size` by its element type's -- which is why every descriptor carries
//! measured bytes alongside the spelling.

/// One field of a struct, or of an enum variant's payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiFieldDesc {
    pub name: &'static str,
    /// The type spelled exactly as written in the source -- `BotId`, `Map`,
    /// `[BotAction; BOTS_MAX]`, `StateOption<Vec2>`, or a bare `T` on a generic impl.
    pub ty: &'static str,
    /// Bytes from the start of the *containing type*, payload offset already folded in for
    /// a variant field.
    pub offset: usize,
    pub size: usize,
    pub align: usize,
}

/// One variant of an enum. `fields` is empty for a fieldless variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiVariantDesc {
    pub name: &'static str,
    /// The discriminant as it appears in the first byte.
    pub tag: u8,
    pub fields: &'static [FfiFieldDesc],
}

/// What shape the type is, and where a reader finds its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiKind {
    Struct {
        fields: &'static [FfiFieldDesc],
    },
    /// A fieldless `#[repr(u8)]` enum: the tag is the whole value.
    UnitEnum {
        variants: &'static [FfiVariantDesc],
    },
    /// A `#[repr(u8, C)]` enum: a `u8` tag at offset 0, then padding, then the payload
    /// union at `payload_offset`. Every variant's payload starts there.
    DataEnum {
        payload_offset: usize,
        variants: &'static [FfiVariantDesc],
    },
}

/// One registered type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiMirrorDesc {
    /// `"SpecialState"`, or `"StateOption<Vec2>"` for a registered instantiation. This is
    /// the key a field's `ty` spelling resolves against.
    pub name: &'static str,
    /// The parameter names on a generic impl (`["T"]`), empty once instantiated.
    pub generic_params: &'static [&'static str],
    /// The arguments a registered instantiation was made with (`["Vec2"]`), empty on the
    /// generic impl itself. Paired with `generic_params` it tells the generator what to
    /// substitute for a field spelled `T`.
    pub generic_args: &'static [&'static str],
    pub size: usize,
    pub align: usize,
    pub kind: FfiKind,
}

/// A `type` alias in the wire closure. A derive cannot be attached to an alias, so the few
/// that exist are registered by hand (`mm_ffi_alias!` in `crate::ffi`) and resolved through
/// the same name table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiAliasDesc {
    pub name: &'static str,
    /// The aliased type, spelled as written.
    pub target: &'static str,
    pub size: usize,
    pub align: usize,
}

/// A `const` that appears as an array length in the wire closure. A derive records a
/// field's type as written (`[BotAction; BOTS_MAX]`) and cannot resolve the length, and
/// size division alone cannot split a *nested* array's dimensions -- `Map` measures 1024
/// bytes whether it is 32x32 or 1024x1, and guessing wrong costs a reader `map[x][y]`. So
/// the handful of lengths that occur are registered by hand (`mm_ffi_const!` in
/// `crate::ffi`) and resolved through their own table.
///
/// The measured sizes stay the cross-check rather than the source: a registered length that
/// disagrees with `field.size / element size` is a test failure, not a silent reshape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiConstDesc {
    pub name: &'static str,
    pub value: usize,
}

/// Implemented by `#[derive(FfiMirror)]`. An associated const rather than a method so a
/// registration can name it in a static initializer, which is what `inventory::submit!`
/// requires -- and so a generic type's descriptor is measured per monomorphization.
pub trait FfiMirrorType {
    const DESC: FfiMirrorDesc;
}

/// One FNV-1a step over the eight bytes of `v`. The hash the layout fingerprint is built
/// from has to be a `const fn` -- it is folded at compile time through associated consts --
/// so this is written out rather than reached for from `core::hash`, none of which is const.
pub const fn mix(hash: u64, v: u64) -> u64 {
    let mut hash = hash;
    let mut i = 0;
    while i < 8 {
        hash ^= (v >> (i * 8)) & 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// The FNV-1a offset basis, and the seed every `LayoutHash` starts from.
pub const HASH_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// Is this byte image a valid value of `Self`?
///
/// Emitted by `#[derive(FfiMirror)]`, which checks every discriminant and every `bool` in the
/// closure; a field with no invalid bit patterns (`f32`, `u32`, `u8`) is vacuously valid, and
/// padding is never looked at. The engine calls it on the raw mapping *before*
/// `EngineChannel::request` materializes a `FleetAction`, because a `bool` of `7` or an
/// out-of-range tag is an invalid value the instant it exists -- and it would exist in the
/// engine's process, not the bot's.
///
/// The recursion is type-directed rather than a walk over [`FfiMirrorDesc`]: a descriptor
/// spells a field's type as a *string*, and resolving that needs the `inventory` registry,
/// which only an `ffi` build has. This check runs in an `engine` build.
pub trait Validate {
    /// `bytes` is at least `size_of::<Self>()` long, and is the raw image of a candidate
    /// value at its natural alignment.
    fn validate(bytes: &[u8]) -> bool;
}

/// A compile-time hash of a type's *measured layout*: size, alignment, every field's offset
/// and size, every variant's tag, mixed with the same for each field type in declaration
/// order.
///
/// Names never enter it. `mm_macros::teams` legitimately renames fields between an `engine`
/// and a `client` build (`GameState::fleet_a` is `GameState::fleet_me` in a bot), so a
/// name-sensitive hash would reject every correct pair. Declaration order is identical in
/// both, which is why no canonical ordering pass is needed.
pub trait LayoutHash {
    const HASH: u64;
}

macro_rules! leaf {
    ($($ty:ty => $seed:literal),* $(,)?) => {$(
        impl Validate for $ty {
            /// Every bit pattern of this width is a valid value.
            fn validate(_bytes: &[u8]) -> bool {
                true
            }
        }

        impl LayoutHash for $ty {
            // The seed separates same-layout leaves: `f32` and `u32` are both four bytes,
            // and swapping one for the other is exactly the kind of break this exists to
            // catch.
            const HASH: u64 = mix(
                mix(HASH_BASIS, $seed),
                (::core::mem::size_of::<$ty>() * 256 + ::core::mem::align_of::<$ty>()) as u64,
            );
        }
    )*};
}

leaf! {
    u8 => 1,
    u16 => 2,
    u32 => 3,
    u64 => 4,
    i8 => 5,
    i16 => 6,
    i32 => 7,
    i64 => 8,
    usize => 9,
    f32 => 10,
    f64 => 11,
}

impl Validate for bool {
    /// The one primitive with invalid bit patterns, and the one a hand-filled buffer
    /// actually gets wrong.
    fn validate(bytes: &[u8]) -> bool {
        bytes[0] <= 1
    }
}

impl LayoutHash for bool {
    const HASH: u64 = mix(mix(HASH_BASIS, 12), 256 + 1);
}

impl<T: Validate, const N: usize> Validate for [T; N] {
    fn validate(bytes: &[u8]) -> bool {
        let stride = ::core::mem::size_of::<T>();
        let mut i = 0;
        while i < N {
            if !T::validate(&bytes[i * stride..]) {
                return false;
            }
            i += 1;
        }
        true
    }
}

impl<T: LayoutHash, const N: usize> LayoutHash for [T; N] {
    const HASH: u64 = mix(mix(mix(HASH_BASIS, 13), N as u64), T::HASH);
}
