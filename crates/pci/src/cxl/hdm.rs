//! The HDM Decoder Capability (CXL 3.1 §8.2.4.20) and the interleave
//! encodings it shares with the ACPI CEDT's CFMWS: ways (IW / ENIW) and
//! granularity (IG / HBIG).

use tock_registers::{LocalRegisterCopy, register_bitfields};

/// HDM Decoder Capability (+0x00, 32-bit), [`Capability`].
pub const CAPABILITY: u32 = 0x00;
/// HDM Decoder Global Control (+0x04, 32-bit), [`GlobalControl`].
pub const GLOBAL_CONTROL: u32 = 0x04;
/// Decoder `n`'s registers start at `+0x10 + 0x20n` ([`Decoder::registers`]).
pub const fn decoder(n: u32) -> u32 {
    0x10 + 0x20 * n
}
/// Offsets inside one decoder's 0x20 bytes.
pub const BASE_LOW: u32 = 0x00;
pub const BASE_HIGH: u32 = 0x04;
pub const SIZE_LOW: u32 = 0x08;
pub const SIZE_HIGH: u32 = 0x0c;
pub const CONTROL: u32 = 0x10;
/// Target list (host bridges, switches) or DPA skip (devices), low/high.
pub const TARGET_LOW: u32 = 0x14;
pub const TARGET_HIGH: u32 = 0x18;

/// Decoder base and size are 256 MiB granular.
pub const ALIGN: u64 = 1 << 28;

register_bitfields![u32,
    /// HDM Decoder Capability +0x00 (32-bit).
    pub Capability [
        /// Encoded decoder count ([`decoder_count`]).
        DECODER_COUNT OFFSET(0) NUMBITS(4) [],
        /// Interleave targets of a host bridge or switch decoder (`0` on devices).
        TARGET_COUNT OFFSET(4) NUMBITS(4) [],
        A11TO8_INTERLEAVE OFFSET(8) NUMBITS(1) [],
        A14TO12_INTERLEAVE OFFSET(9) NUMBITS(1) [],
        POISON_ON_DECODE_ERROR OFFSET(10) NUMBITS(1) [],
        WAYS_3_6_12 OFFSET(11) NUMBITS(1) [],
        WAYS_16 OFFSET(12) NUMBITS(1) [],
        UIO OFFSET(13) NUMBITS(1) [],
        UIO_DECODER_COUNT OFFSET(16) NUMBITS(4) [],
        MEMDATA_NXM OFFSET(20) NUMBITS(1) []
    ],
    /// HDM Decoder Global Control +0x04 (32-bit).
    pub GlobalControl [
        POISON_ON_DECODE_ERROR_ENABLE OFFSET(0) NUMBITS(1) [],
        ENABLE OFFSET(1) NUMBITS(1) []
    ],
    /// HDM Decoder n Control +0x20 + 0x20n (32-bit).
    pub Control [
        /// Interleave granularity ([`granularity`]).
        IG OFFSET(0) NUMBITS(4) [],
        /// Interleave ways ([`ways`]).
        IW OFFSET(4) NUMBITS(4) [],
        LOCK_ON_COMMIT OFFSET(8) NUMBITS(1) [],
        COMMIT OFFSET(9) NUMBITS(1) [],
        COMMITTED OFFSET(10) NUMBITS(1) [],
        ERROR_NOT_COMMITTED OFFSET(11) NUMBITS(1) [],
        TARGET_TYPE OFFSET(12) NUMBITS(1) [
            /// HDM-D/HDM-DB: device-coherent memory (type 2).
            DeviceCoherent = 0,
            /// HDM-H: host-only coherent memory (type 3 expanders).
            HostOnly = 1
        ],
        BI OFFSET(13) NUMBITS(1) [],
        UIO OFFSET(14) NUMBITS(1) []
    ]
];

/// The Decoder Count field for `n` decoders (1, 2, 4, 6, 8, 10, 12, 14,
/// 16, 20, 24, 28, 32); a count between two encodings rounds up.
pub const fn decoder_count(n: u8) -> Option<u8> {
    Some(match n {
        0 | 1 => 0,
        2 => 1,
        3 | 4 => 2,
        5..=16 => n.div_ceil(2),
        17..=32 => 8 + (n - 16).div_ceil(4),
        _ => return None,
    })
}

/// The decoders a Decoder Count field `field` stands for.
pub const fn decoders(field: u8) -> Option<u8> {
    Some(match field {
        0 => 1,
        1..=8 => 2 * field,
        9..=12 => 16 + 4 * (field - 8),
        _ => return None,
    })
}

/// The IW / ENIW encoding of `ways` (1, 2, 4, 8, 16, 3, 6, 12).
pub const fn ways(ways: u8) -> Option<u8> {
    Some(match ways {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        16 => 4,
        3 => 8,
        6 => 9,
        12 => 0xa,
        _ => return None,
    })
}

/// The ways of an IW / ENIW encoding.
pub const fn ways_of(encoded: u8) -> Option<u8> {
    Some(match encoded {
        0..=4 => 1 << encoded,
        8..=0xa => 3 << (encoded - 8),
        _ => return None,
    })
}

/// The IG / HBIG encoding of `bytes` (256 B .. 16 KiB).
pub const fn granularity(bytes: u32) -> Option<u8> {
    if !bytes.is_power_of_two() || bytes < 256 || bytes > 16 * 1024 {
        return None;
    }
    Some((bytes.trailing_zeros() - 8) as u8)
}

/// The bytes of an IG / HBIG encoding.
pub const fn granularity_bytes(encoded: u8) -> Option<u32> {
    if encoded > 6 {
        return None;
    }
    Some(256 << encoded)
}

/// One HDM decoder as firmware commits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoder {
    pub base: u64,
    pub size: u64,
    /// Interleave granularity in bytes.
    pub granularity: u32,
    /// Target port IDs, way 0 first: the host bridge's or switch's
    /// downstream ports (their PCIe Link Capabilities Port Number).
    pub targets: heapless::Vec<u8, 8>,
    pub target_type: TargetType,
    pub lock_on_commit: bool,
    /// Report the decoder as committed (an emulated host bridge, whose
    /// decoders nobody commits).
    pub committed: bool,
}

/// The memory a decoder routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    DeviceCoherent,
    HostOnly,
}

/// Why a [`Decoder`] cannot be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Base or size not 256 MiB aligned, or the range wraps.
    Alignment,
    Ways(usize),
    Granularity(u32),
}

impl Decoder {
    /// The decoder's seven registers from [`BASE_LOW`] to [`TARGET_HIGH`].
    pub fn registers(&self) -> Result<[u32; 7], Error> {
        if !self.base.is_multiple_of(ALIGN)
            || !self.size.is_multiple_of(ALIGN)
            || self.base.checked_add(self.size).is_none()
        {
            return Err(Error::Alignment);
        }
        let ways_count = self.targets.len().max(1);
        let iw = u8::try_from(ways_count)
            .ok()
            .and_then(ways)
            .ok_or(Error::Ways(ways_count))?;
        let ig = granularity(self.granularity).ok_or(Error::Granularity(self.granularity))?;
        let mut control = LocalRegisterCopy::<u32, Control::Register>::new(0);
        control.write(
            Control::IG.val(ig.into())
                + Control::IW.val(iw.into())
                + Control::LOCK_ON_COMMIT.val(self.lock_on_commit.into())
                + Control::COMMITTED.val((self.committed && self.size != 0).into())
                + match self.target_type {
                    TargetType::DeviceCoherent => Control::TARGET_TYPE::DeviceCoherent,
                    TargetType::HostOnly => Control::TARGET_TYPE::HostOnly,
                },
        );
        let mut list = [0u8; 8];
        list[..self.targets.len()].copy_from_slice(&self.targets);
        let [t0, t1, t2, t3, t4, t5, t6, t7] = list;
        Ok([
            self.base as u32,
            (self.base >> 32) as u32,
            self.size as u32,
            (self.size >> 32) as u32,
            control.get(),
            u32::from_le_bytes([t0, t1, t2, t3]),
            u32::from_le_bytes([t4, t5, t6, t7]),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_encodings_follow_the_spec() {
        for (w, e) in [
            (1, 0),
            (2, 1),
            (4, 2),
            (8, 3),
            (16, 4),
            (3, 8),
            (6, 9),
            (12, 0xa),
        ] {
            assert_eq!(ways(w), Some(e));
            assert_eq!(ways_of(e), Some(w));
        }
        assert_eq!(ways(5), None);
        assert_eq!(granularity(256), Some(0));
        assert_eq!(granularity(4096), Some(4));
        assert_eq!(granularity(16384), Some(6));
        assert_eq!(granularity(32768), None);
        assert_eq!(granularity_bytes(2), Some(1024));
        // AMD's emulated host bridge: 1 -> 0, 2 -> 1, 3/4 -> 2.
        assert_eq!(decoder_count(1), Some(0));
        assert_eq!(decoder_count(2), Some(1));
        assert_eq!(decoder_count(3), Some(2));
        assert_eq!(decoder_count(4), Some(2));
        assert_eq!(decoder_count(32), Some(12));
        for field in 0..=12 {
            assert_eq!(decoder_count(decoders(field).unwrap()), Some(field));
        }
    }

    #[test]
    fn committed_host_only_decoder_matches_the_amd_emulation() {
        let decoder = Decoder {
            base: 0x40_0000_0000,
            size: 0x10_0000_0000,
            granularity: 256,
            targets: heapless::Vec::from_slice(&[3]).unwrap(),
            target_type: TargetType::HostOnly,
            lock_on_commit: true,
            committed: true,
        };
        let regs = decoder.registers().unwrap();
        assert_eq!(regs[..4], [0, 0x40, 0, 0x10]);
        // IG | IW << 4 | 0x1100 | 0x400 (CxlV2Manager `CxlPopulateHDMDecoder`).
        assert_eq!(regs[4], 0x1500);
        assert_eq!(regs[5..], [3, 0]);
        let unaligned = Decoder {
            base: 0x1000,
            ..decoder.clone()
        };
        assert_eq!(unaligned.registers(), Err(Error::Alignment));
    }
}
