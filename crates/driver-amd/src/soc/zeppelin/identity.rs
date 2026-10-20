// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin CPU identification.

/// CPUID Fn8000_0001 EBX[31:28].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    /// EPYC Embedded 3000, two dies.
    Sp4,
    Am4,
    /// EPYC Embedded 3000, one die.
    Sp4r2,
    Sp3,
    Unknown(u8),
}
impl Package {
    pub const fn from_cpuid(extended_ebx: u32) -> Self {
        match extended_ebx >> 28 {
            1 => Self::Sp4,
            2 => Self::Am4,
            3 => Self::Sp4r2,
            4 => Self::Sp3,
            other => Self::Unknown(other as u8),
        }
    }
    /// Dies a supported package has.
    pub const fn dies(self) -> Option<u8> {
        match self {
            Self::Sp4 => Some(2),
            Self::Sp4r2 => Some(1),
            _ => None,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sp4 => "SP4",
            Self::Am4 => "AM4",
            Self::Sp4r2 => "SP4r2",
            Self::Sp3 => "SP3",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// Logical revision, as the vendor firmware's register tables use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision {
    A0,
    B0,
    B1,
    B2,
}
impl Revision {
    /// From CPUID Fn8000_0001 EAX. The vendor firmware treats an unknown
    /// stepping as B2; fail closed instead.
    pub const fn from_cpuid(extended_eax: u32) -> Option<Self> {
        if extended_eax & 0x0fff_0f00 != 0x0080_0f00 {
            return None;
        }
        match ((extended_eax >> 4) & 0xf, extended_eax & 0xf) {
            (0, 0) => Some(Self::A0),
            (1, 0) => Some(Self::B0),
            (1, 1) => Some(Self::B1),
            (1, 2) => Some(Self::B2),
            _ => None,
        }
    }
    /// The revision bit of the vendor register-table predicates.
    pub const fn mask(self) -> u8 {
        1 << self as u8
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::A0 => "A0",
            Self::B0 => "B0",
            Self::B1 => "B1",
            Self::B2 => "B2",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_epyc_3151() {
        // EPYC 3151: family 17h model 1 stepping 2, SP4r2.
        assert_eq!(Revision::from_cpuid(0x00800f12), Some(Revision::B2));
        assert_eq!(Revision::from_cpuid(0x00810f10), None);
        assert_eq!(Package::from_cpuid(0x3000_0000).dies(), Some(1));
        assert_eq!(Revision::B1.mask(), 4);
    }
}
