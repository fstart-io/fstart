use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};

/// Const-buildable fixed-capacity vector for POD firmware config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstVec<T, const N: usize> {
    items: [T; N],
    len: usize,
}

impl<T: Copy, const N: usize> ConstVec<T, N> {
    #[must_use]
    pub const fn new(fill: T) -> Self {
        Self {
            items: [fill; N],
            len: 0,
        }
    }

    #[must_use]
    pub const fn push(mut self, item: T) -> Self {
        if self.len >= N {
            panic!("ConstVec capacity exceeded");
        }
        self.items[self.len] = item;
        self.len += 1;
        self
    }

    #[must_use]
    pub const fn get(&self, index: usize) -> T {
        if index >= self.len {
            panic!("ConstVec index out of bounds");
        }
        self.items[index]
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.items[..self.len]
    }
}

impl<T: Default, const N: usize> Default for ConstVec<T, N> {
    fn default() -> Self {
        Self {
            items: core::array::from_fn(|_| T::default()),
            len: 0,
        }
    }
}

impl<T: Serialize, const N: usize> Serialize for ConstVec<T, N> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("ConstVec", 2)?;
        state.serialize_field("items", &Items(&self.items))?;
        state.serialize_field("len", &self.len)?;
        state.end()
    }
}

struct Items<'a, T, const N: usize>(&'a [T; N]);

impl<T: Serialize, const N: usize> Serialize for Items<'_, T, N> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(N))?;
        for item in self.0 {
            seq.serialize_element(item)?;
        }
        seq.end()
    }
}
