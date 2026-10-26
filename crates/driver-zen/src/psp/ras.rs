//! Rome PSP RAS-EINJ wire contract (C2P 22h).
//!
//! Recovered from S8030 V4.04 AmdPlatformRasSspSmm
//! f8356c42-4ba6-706f-59e9-a9b1aff2bcd8, SHA256 cb5c20f9dd3c…:
//! 6e90 (message), 1ee0/1f38 (begin/end), 1d54 (memory setup).
//! Only correctable DRAM injection is exposed here. This is not an MCA
//! register injector: the PSP programs the memory-controller backend.

use tock_registers::register_bitfields;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

register_bitfields![u32,
    // Names: Rome CBS setup help and OpenSIL Ras.h register facts.
    UMC_MISC_CFG [DIS_ERR_INJ OFFSET(2) NUMBITS(1) []],
    UMC_ECC_CTRL [RD_ECC_EN OFFSET(10) NUMBITS(1) []],
    UMC_BASE [EN OFFSET(0) NUMBITS(1) []],
    CS_HASH [EN OFFSET(0) NUMBITS(1) [], ADDR_MASK OFFSET(1) NUMBITS(31) []]
];

use super::command::Command;
use crate::soc::Generation;

/// Control actions: 1 enables injection, 2 begins, 3 ends an operation.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct Control {
    action: u32,
}

impl Control {
    pub const ENABLE: Self = Self { action: 1 };
    pub const BEGIN: Self = Self { action: 2 };
    pub const END: Self = Self { action: 3 };
}

impl Command for Control {
    const ID: u8 = 0x22;
    const NAME: &'static str = "RasEinj";

    fn supported(generation: Generation) -> bool {
        generation == Generation::Rome
    }
}

/// Setup action 4. Packed because the address starts at wire offset 12,
/// immediately after the 8-byte header and 4-byte action, not offset 16.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct CorrectableMemory {
    action: u32,
    address: u64,
    umc_base: u32,
    kind: u16,
    chip_select: u8,
    socket: u8,
}

impl CorrectableMemory {
    /// `address` must be an owned, disposable, cache-line-aligned physical
    /// DRAM address. `target` must be decoded from the current DRAM map.
    pub const fn new(address: u64, target: MemoryTarget) -> Self {
        Self {
            action: 4,
            address,
            umc_base: 0x50000 + (target.channel as u32) * 0x100000,
            kind: 1,
            chip_select: target.chip_select,
            socket: 0,
        }
    }
}

impl Command for CorrectableMemory {
    const ID: u8 = 0x22;
    const NAME: &'static str = "RasEinj";

    fn supported(generation: Generation) -> bool {
        generation == Generation::Rome
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryTarget {
    pub channel: u8,
    pub chip_select: u8,
}

/// Decode a low-4-GiB Rome target on socket 0. `df(instance, offset)` reads
/// F0, `smn(address)` reads the socket's SMN. This deliberately accepts a
/// subset: zero-based rule 0, no die/socket interleave, power-of-two or
/// COD2/4/8 channel interleave. Other maps fail closed, not approximately.
///
/// Selection/normalization: AmdRasSspSmm ee8175ef… 3a90/4180; chip-select
/// match and hash: 3300, with live UMC register offsets from AmdRasSspDxe
/// f71aab75… 4bc. Callers must separately establish a single-socket Rome.
#[cfg(feature = "rome")]
pub fn memory_target(
    address: u64,
    df: impl Fn(u8, u16) -> u32,
    smn: impl Fn(u32) -> u32,
) -> Option<MemoryTarget> {
    use crate::df::v3::{DRAM_BASE, DRAM_LIMIT};
    let base = df(0, 0x110);
    let limit = df(0, 0x114);
    if address >= 1 << 32
        || !DRAM_BASE::VALID.is_set(base)
        || DRAM_BASE::BASE.read(base) != 0
        || DRAM_BASE::ILV_DIE.read(base) != 0
        || DRAM_BASE::ILV_SOCK.is_set(base)
        || address >= (u64::from(DRAM_LIMIT::LIMIT.read(limit)) + 1) << 28
    {
        return None;
    }
    let start = DRAM_BASE::ILV_ADDR.read(base) + 8;
    if start > 12 {
        return None;
    }
    let mode = DRAM_BASE::ILV_CHAN.read(base);
    let bits = match mode {
        0 => 0,
        1 | 0xc => 1,
        3 | 0xd => 2,
        5 | 0xe => 3,
        _ => return None,
    };
    let high = df(0, 0x118);
    if (DRAM_BASE::HOLE_EN.is_set(base) && address >= u64::from(df(0, 0x104) & 0xff00_0000))
        || (DRAM_BASE::VALID.is_set(high)
            && address >= (u64::from(DRAM_BASE::BASE.read(high)) << 28))
    {
        return None;
    }
    let hash = df(0, 0x3f8);
    let bit = |n| ((address >> n) & 1) as u32;
    let selected = if mode >= 0xc {
        let h = |n: u32, shift: u32| bit(n) & ((hash >> shift) & 1u32);
        (bit(start) ^ bit(14) ^ h(18, 20) ^ h(23, 21) ^ h(32, 22))
            | if bits >= 2 {
                (bit(12) ^ h(16, 20) ^ h(21, 21) ^ h(30, 22)) << 1
            } else {
                0
            }
            | if bits >= 3 {
                (bit(13) ^ h(17, 20) ^ h(22, 21) ^ h(31, 22)) << 2
            } else {
                0
            }
    } else {
        ((address >> start) & ((1 << bits) - 1)) as u32
    };
    let channel = DRAM_LIMIT::DST.read(limit).checked_add(selected)?;
    if channel >= 8 {
        return None;
    }
    let channel = channel as u8;
    // The selected UMC must describe the same rule. Rule 1 / high offsets
    // are not handled, even if their global mapping would select this UMC.
    if df(channel, 0x110) != base || df(channel, 0x114) != limit {
        return None;
    }
    let normalized = if mode >= 0xd {
        let middle_bits = 11u32.checked_sub(start)?;
        (address & ((1 << start) - 1))
            | (((address >> (start + 1)) & ((1 << middle_bits) - 1)) << start)
            | ((address >> (11 + bits)) << 11)
    } else {
        (address & ((1 << start) - 1)) | ((address >> (start + bits)) << start)
    };
    let umc = u32::from(channel) * 0x100000 + 0x50000;
    // Exact donor preconditions: DisErrInj clear, ECC checking enabled.
    // DisErrInj is a memory-init/APCB policy, not a live override here.
    if UMC_MISC_CFG::DIS_ERR_INJ.is_set(smn(umc + 0x1e0))
        || !UMC_ECC_CTRL::RD_ECC_EN.is_set(smn(umc + 0x14c))
    {
        return None;
    }
    let matches = (0..4u8).filter(|&cs| {
        let mask = smn(umc + 0x20 + u32::from(cs / 2) * 4) | 1;
        let base = smn(umc + u32::from(cs) * 4);
        // Secondary ranges are not supported, rather than silently using
        // an enabled secondary chip-select as a primary one.
        let secondary = smn(umc + 0x10 + u32::from(cs) * 4);
        UMC_BASE::EN.is_set(base)
            && !UMC_BASE::EN.is_set(secondary)
            && ((normalized >> 8) as u32 & !mask) == (base & !mask)
    });
    let mut matches = matches;
    let cs = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let hash_bit = |offset| {
        let h = smn(umc + offset);
        if !CS_HASH::EN.is_set(h) {
            0
        } else {
            (((normalized >> 9) as u32 & CS_HASH::ADDR_MASK.read(h)).count_ones() & 1) as u8
        }
    };
    Some(MemoryTarget {
        channel,
        chip_select: cs ^ hash_bit(0xe8) ^ (hash_bit(0xec) << 1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_wire_layout() {
        let c = CorrectableMemory::new(
            0x1234_5000,
            MemoryTarget {
                channel: 5,
                chip_select: 2,
            },
        );
        assert_eq!(size_of::<Control>(), 4);
        assert_eq!(size_of::<CorrectableMemory>(), 20);
        assert_eq!(
            c.as_bytes(),
            &[
                4, 0, 0, 0, 0, 0x50, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 0x55, 0, 1, 0, 2, 0,
            ]
        );
        assert!(!Control::supported(Generation::Milan));
    }

    #[test]
    fn smm_mailbox_sequence_and_psp_failure() {
        use crate::psp::command::{C2pBuffer, Header};
        use crate::psp::mailbox::{
            Mailbox, PspError,
            model::{Model, NoDelay},
        };
        use std::{cell::RefCell, rc::Rc};
        let actions = Rc::new(RefCell::new(std::vec::Vec::new()));
        let seen = actions.clone();
        let mut flag = 0u8;
        let flag_ptr = core::ptr::from_mut(&mut flag);
        let model = Model::new(0x8000_0000, move |id, address| {
            assert_eq!(id, 0x22);
            // Model completion is synchronous, while the caller owns the
            // live buffer and SMM flag.
            unsafe {
                assert_eq!(flag_ptr.read_volatile(), 1);
                let header = (address as *const Header).read();
                let action = ((address + 8) as *const u32).read();
                assert_eq!(header.size, if action == 4 { 28 } else { 12 });
                seen.borrow_mut().push(action);
                if action == 4 { 5 } else { 0 }
            }
        });
        let m = Mailbox::new(&model, Generation::Rome);
        for control in [Control::ENABLE, Control::BEGIN] {
            unsafe { m.send_in_smm(&NoDelay, &mut C2pBuffer::new(control), flag_ptr) }.unwrap();
            assert_eq!(flag, 0);
        }
        let request = CorrectableMemory::new(
            0x1234_5000,
            MemoryTarget {
                channel: 0,
                chip_select: 0,
            },
        );
        assert_eq!(
            unsafe { m.send_in_smm(&NoDelay, &mut C2pBuffer::new(request), flag_ptr) },
            Err(PspError::Status {
                command: 0x22,
                status: 5
            })
        );
        assert_eq!(flag, 0);
        unsafe { m.send_in_smm(&NoDelay, &mut C2pBuffer::new(Control::END), flag_ptr) }.unwrap();
        assert_eq!(*actions.borrow(), [1, 2, 4, 3]);
    }

    #[cfg(feature = "rome")]
    #[test]
    fn live_target_selection_and_rejections() {
        // Captured on the Rome 7502P bench, 2026-10-26: COD8, start
        // bit 8, hash bits 23/32 enabled, hole at 2.75 GiB, one 16-GiB CS.
        let base = 0x3b;
        let df = |_: u8, off| match off {
            0x110 => base,
            0x114 => 0x0020_4000,
            0x104 => 0xb000_0001,
            0x3f8 => 0x0066_6600,
            _ => 0,
        };
        let smn = |off: u32| match off & 0xfffff {
            0x5014c => 0x673,
            0x501e0 => 0x113,
            0x50000 => 1,
            0x50020 => 0x03ff_fffe,
            0x500e8 | 0x500ec => 0x03ff_fe00,
            _ => 0,
        };
        let target = memory_target(0x75bf_1000, df, smn).unwrap();
        assert_eq!(target.channel, 3);
        assert_eq!(target.chip_select, 0);
        assert_eq!(memory_target(1 << 32, df, smn), None);
        assert_eq!(
            memory_target(
                0,
                |_, o| if o == 0x110 {
                    base | (1 << 8)
                } else {
                    df(0, o)
                },
                smn
            ),
            None
        );
        assert_eq!(memory_target(0, df, |_| 0), None);
        // The original APCB sets DisErrInj; PSP enable alone must not
        // bypass it. The boot must permit injection through memory init.
        assert_eq!(
            memory_target(0x75bf_1000, df, |o| if o & 0xfffff == 0x501e0 {
                0x117
            } else {
                smn(o)
            }),
            None
        );
        assert_eq!(
            memory_target(0, df, |o| if o & 0xfffff == 0x50004 { 1 } else { smn(o) }),
            None
        );
    }
}
