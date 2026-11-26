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

/// How long a commit may take: the vendors stall 10 ms after Commit
/// before checking Committed (`CxlMgrEndpointDecoderProgramming`).
pub const COMMIT_TIMEOUT_US: u64 = 10_000;

/// What an endpoint or switch decoder is committed with: the window, its
/// interleave and, for a switch's upstream port, the target list (a
/// device's DPA skip stays 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    pub base: u64,
    pub size: u64,
    pub ways: u8,
    /// Interleave granularity in bytes.
    pub granularity: u32,
    /// Target List Low/High: way `i` in byte `i` (CXL 3.1 §8.2.4.20.11).
    pub targets: Option<[u8; 8]>,
    /// Ask for Lock On Commit (kept only if [`Hdm::lock_allowed`]).
    pub lock: bool,
}

/// Why a commit failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitError {
    Ways(u8),
    Granularity(u32),
    /// No such decoder in the capability.
    Decoder(u32),
    /// Committed never set, or Error Not Committed did (the vendor's
    /// `0x434C_0909` for devices, `0x434C_0A03` for switches).
    NotCommitted,
}

/// The HDM Decoder Capability at `base` of a register block.
pub struct Hdm<R> {
    regs: R,
    base: u32,
}

impl<R: super::Registers> Hdm<R> {
    /// The capability at `base` (its offset in `regs`).
    pub const fn new(regs: R, base: u32) -> Self {
        Self { regs, base }
    }

    /// The HDM decoder capability of a component register block `regs`
    /// (the CXL.cache/mem array at 0x1000), if it has one.
    pub fn of_component(regs: R) -> Option<Self> {
        let cache_mem = super::component::CACHE_MEM;
        let (offset, _) = super::component::find(
            |o| regs.read32(cache_mem + o),
            super::component::id::HDM_DECODER,
        )?;
        Some(Self::new(regs, cache_mem + offset))
    }

    pub fn capability(&self) -> LocalRegisterCopy<u32, Capability::Register> {
        LocalRegisterCopy::new(self.regs.read32(self.base + CAPABILITY))
    }

    /// The decoders the capability has.
    pub fn decoder_count(&self) -> u8 {
        decoders(self.capability().read(Capability::DECODER_COUNT) as u8).unwrap_or(0)
    }

    fn reg(&self, n: u32, offset: u32) -> u32 {
        self.base + decoder(n) + offset
    }

    pub fn control(&self, n: u32) -> LocalRegisterCopy<u32, Control::Register> {
        LocalRegisterCopy::new(self.regs.read32(self.reg(n, CONTROL)))
    }

    /// Decoder `n`'s base and size as programmed.
    pub fn range(&self, n: u32) -> (u64, u64) {
        let pair = |low, high| {
            u64::from(self.regs.read32(self.reg(n, high))) << 32
                | u64::from(self.regs.read32(self.reg(n, low)) & 0xf000_0000)
        };
        (pair(BASE_LOW, BASE_HIGH), pair(SIZE_LOW, SIZE_HIGH))
    }

    /// The vendor's checks before Lock On Commit of decoder `n` at
    /// `commit` (`CxlMgrEndpointDecoderProgramming`): above the previous
    /// decoder's end, no wrap, a multiple of 3 for 3/6/12 ways, the
    /// previous decoder committed.
    pub fn lock_allowed(&self, n: u32, commit: &Commit) -> bool {
        if commit.base.checked_add(commit.size).is_none() {
            return false;
        }
        if ways(commit.ways).is_some_and(|iw| iw >= 8) && !commit.size.is_multiple_of(3) {
            return false;
        }
        if n == 0 {
            return true;
        }
        let (base, size) = self.range(n - 1);
        self.control(n - 1).is_set(Control::COMMITTED) && commit.base >= base.saturating_add(size)
    }

    /// Program and commit decoder `n` (CXL 3.1 §8.2.4.20.12, the vendors'
    /// order): base and size, then Control as read with IW/IG (IW 0 for
    /// one way, which the vendors leave as read: a decoder an earlier owner
    /// left interleaved would commit with its stale ways), Lock On Commit
    /// when asked and allowed, Commit; then wait for Committed. Target
    /// Type and the other bits stay as read.
    pub fn commit(
        &self,
        n: u32,
        commit: &Commit,
        timer: &dyn fstart_core::services::Timer,
    ) -> Result<(), CommitError> {
        if n >= u32::from(self.decoder_count()) {
            return Err(CommitError::Decoder(n));
        }
        let iw = ways(commit.ways).ok_or(CommitError::Ways(commit.ways))?;
        let ig =
            granularity(commit.granularity).ok_or(CommitError::Granularity(commit.granularity))?;
        let lock = commit.lock && self.lock_allowed(n, commit);
        let mask = |v: u64| (v as u32) & 0xf000_0000;
        self.regs.write32(self.reg(n, BASE_LOW), mask(commit.base));
        self.regs
            .write32(self.reg(n, BASE_HIGH), (commit.base >> 32) as u32);
        self.regs.write32(self.reg(n, SIZE_LOW), mask(commit.size));
        self.regs
            .write32(self.reg(n, SIZE_HIGH), (commit.size >> 32) as u32);
        if let Some(t) = commit.targets {
            self.regs.write32(
                self.reg(n, TARGET_LOW),
                u32::from_le_bytes([t[0], t[1], t[2], t[3]]),
            );
            self.regs.write32(
                self.reg(n, TARGET_HIGH),
                u32::from_le_bytes([t[4], t[5], t[6], t[7]]),
            );
        }
        let mut control = self.control(n);
        control.modify(Control::IW.val(iw.into()) + Control::IG.val(ig.into()));
        if lock {
            control.modify(Control::LOCK_ON_COMMIT::SET);
        }
        control.modify(Control::COMMIT::SET);
        self.regs.write32(self.reg(n, CONTROL), control.get());
        let done = fstart_core::services::timer::poll_until(timer, COMMIT_TIMEOUT_US, || {
            let c = self.control(n);
            (c.is_set(Control::COMMITTED) || c.is_set(Control::ERROR_NOT_COMMITTED)).then_some(c)
        });
        match done {
            Some(c) if c.is_set(Control::COMMITTED) && !c.is_set(Control::ERROR_NOT_COMMITTED) => {
                Ok(())
            }
            _ => Err(CommitError::NotCommitted),
        }
    }

    /// HDM Decoder Global Control's Enable (`+4 |= 2`; the vendor's switch
    /// path ORs it into `+4 + 0x20n` instead, a bug fstart does not keep).
    pub fn enable(&self) {
        let at = self.base + GLOBAL_CONTROL;
        let mut control = tock_registers::LocalRegisterCopy::<u32, GlobalControl::Register>::new(
            self.regs.read32(at),
        );
        control.modify(GlobalControl::ENABLE::SET);
        self.regs.write32(at, control.get());
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

    /// A device's HDM capability at 0x100 of a block whose decoders commit
    /// at once unless `fail`.
    struct Block {
        regs: core::cell::RefCell<[u32; 0x100]>,
        fail: bool,
    }

    impl crate::cxl::Registers for &Block {
        fn read32(&self, offset: u32) -> u32 {
            self.regs.borrow()[offset as usize / 4]
        }
        fn write32(&self, offset: u32, value: u32) {
            let mut v = value;
            // Decoder n's control at 0x120 + 0x20n.
            if offset >= 0x120 && (offset - 0x120).is_multiple_of(0x20) && v & 1 << 9 != 0 {
                v |= if self.fail { 1 << 11 } else { 1 << 10 };
            }
            self.regs.borrow_mut()[offset as usize / 4] = v;
        }
    }

    #[test]
    fn endpoint_decoders_commit_with_the_vendor_checks() {
        use fstart_core::services::timer::ManualTimer;
        let block = Block {
            regs: core::cell::RefCell::new([0; 0x100]),
            fail: false,
        };
        // Two decoders; decoder 0's control has Target Type set.
        block.regs.borrow_mut()[0x100 / 4] = 1;
        block.regs.borrow_mut()[(0x100 + 0x20) / 4] = 1 << 12;
        let hdm = Hdm::new(&block, 0x100);
        assert_eq!(hdm.decoder_count(), 2);
        let timer = ManualTimer::ticking();
        let commit = Commit {
            base: 0x40_0000_0000,
            size: 0x4000_0000,
            ways: 2,
            granularity: 4096,
            targets: None,
            lock: true,
        };
        hdm.commit(0, &commit, &timer).unwrap();
        let control = block.regs.borrow()[0x120 / 4];
        // IG 4, IW 1, LockOnCommit, Commit, Committed, Target Type kept.
        assert_eq!(control, 4 | 1 << 4 | 1 << 8 | 1 << 9 | 1 << 10 | 1 << 12);
        assert_eq!(hdm.range(0), (0x40_0000_0000, 0x4000_0000));
        // Decoder 1 below decoder 0's end: committed, but not locked.
        let below = Commit {
            base: 0x10_0000_0000,
            ..commit
        };
        assert!(!hdm.lock_allowed(1, &below));
        hdm.commit(1, &below, &timer).unwrap();
        assert_eq!(block.regs.borrow()[0x140 / 4] & 1 << 8, 0);
        assert_eq!(hdm.commit(2, &commit, &timer), Err(CommitError::Decoder(2)));
        hdm.enable();
        assert_eq!(block.regs.borrow()[0x104 / 4], 2);
        // 3-way needs a size divisible by 3 to lock.
        assert!(!hdm.lock_allowed(0, &Commit { ways: 3, ..commit }));
        // One way: a stale interleave left in decoder 1 is cleared.
        block.regs.borrow_mut()[0x140 / 4] = 2 << 4 | 3;
        let one = Commit {
            ways: 1,
            granularity: 256,
            lock: false,
            ..below
        };
        hdm.commit(1, &one, &timer).unwrap();
        assert_eq!(block.regs.borrow()[0x140 / 4] & 0xff, 0);

        let failing = Block {
            regs: core::cell::RefCell::new([0; 0x100]),
            fail: true,
        };
        failing.regs.borrow_mut()[0x100 / 4] = 1;
        let hdm = Hdm::new(&failing, 0x100);
        assert_eq!(
            hdm.commit(0, &commit, &timer),
            Err(CommitError::NotCommitted)
        );
    }
}
