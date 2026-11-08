//! Bounded metadata constructors shared by board crates and host tooling.

use heapless::{String as HString, Vec as HVec};

/// Construct a bounded heapless string for static board metadata.
///
/// This keeps board crates from each defining their own `HString::try_from`
/// wrappers while still failing fast when a literal exceeds the schema capacity.
#[must_use]
pub fn hstr<const N: usize>(value: &str) -> HString<N> {
    HString::try_from(value).expect("string exceeds heapless capacity")
}

/// Construct a bounded heapless vector for static board metadata.
///
/// The output capacity `C` is inferred from the destination field type.
#[must_use]
pub fn hvec<T, const N: usize, const C: usize>(items: [T; N]) -> HVec<T, C> {
    let mut out = HVec::new();
    for item in items {
        out.push(item).ok().expect("heapless vec capacity");
    }
    out
}
