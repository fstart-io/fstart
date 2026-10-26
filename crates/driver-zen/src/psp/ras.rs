//! Rome PSP RAS-EINJ (C2P 22h), including socket selection.
//!
//! S8030 V4.04 AmdPlatformRasSspSmm f8356c42-4ba6-706f-59e9-a9b1aff2bcd8:
//! 6e90 message, 1d54 memory setup. S8253 V2.03 (2P), same GUID:
//! 6e80 message, 1d4c setup. The primary PSP receives a socket in the
//! payload; x86 UMC/NBIO accesses must use that socket's SMN endpoint.

use super::command::Command;
use crate::soc::Generation;
use tock_registers::register_bitfields;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

register_bitfields![u32,
    UMC_MISC_CFG [DIS_ERR_INJ OFFSET(2) NUMBITS(1) []],
    UMC_ECC_CTRL [RD_ECC_EN OFFSET(10) NUMBITS(1) []],
    UMC_BASE [EN OFFSET(0) NUMBITS(1) []],
    CS_HASH [EN OFFSET(0) NUMBITS(1) [], ADDR_MASK OFFSET(1) NUMBITS(31) []],
    // DF3 F1x208 / F1x20c, broadcast. Names are public register facts.
    FABRIC_MASK0 [COMPONENT OFFSET(0) NUMBITS(10) [], NODE OFFSET(16) NUMBITS(10) []],
    FABRIC_MASK1 [NODE_SHIFT OFFSET(0) NUMBITS(4) [], SOCKET_SHIFT OFFSET(8) NUMBITS(2) [], SOCKET OFFSET(24) NUMBITS(3) []]
];

pub const MAX_SOCKETS: usize = 2;
/// Instance selector for a node-broadcast indirect read (TARGET_INST=0).
pub const BROADCAST: u8 = 0xff;

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

/// Packed: the u64 physical address is at wire offset 12, not 16.
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
    /// The caller must own this disposable physical cache line. `target`
    /// must be decoded from the live fabric and UMC configuration.
    pub const fn new(address: u64, target: MemoryTarget) -> Self {
        Self {
            action: 4,
            address,
            umc_base: 0x50000 + (target.channel as u32) * 0x100000,
            kind: 1,
            chip_select: target.chip_select,
            socket: target.socket,
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
    pub socket: u8,
    pub channel: u8,
    pub chip_select: u8,
}

#[cfg(feature = "rome")]
fn channel_bits(base: u32) -> Option<u32> {
    use crate::df::v3::DRAM_BASE;
    match DRAM_BASE::ILV_CHAN.read(base) {
        0 => Some(0),
        1 | 0xc => Some(1),
        3 | 0xd => Some(2),
        5 | 0xe => Some(3),
        _ => None,
    }
}

/// Decode physical address → socket/UMC/CS using the global CCM map,
/// live fabric-ID masks, and the selected UMC's local normalization rules.
/// `df(socket, instance, reg)` accepts BROADCAST as well as real instances.
/// `smn(socket, address)` must reach that socket, not always bus 0.
///
/// Supported: 1P/2P, separate or socket-interleaved RAM, nonzero rule bases,
/// DRAM-hole remapping, local rule-1 offsets, power-of-two/COD2/4/8 channel
/// interleave, and primary CS ranges. Die/6-channel interleave and ambiguous
/// or unsupported maps fail closed. Target addresses are not dereferenced.
///
/// Evidence: S8253 2P AmdRasSspSmm ee8175ef… 3a70 (selection), 4160
/// (normalization), 32e0 (CS); corresponding S8030 3a90/4180/3300.
#[cfg(feature = "rome")]
pub fn memory_target(
    address: u64,
    sockets: u8,
    df: impl Fn(u8, u8, crate::df::Reg) -> u32,
    smn: impl Fn(u8, u32) -> u32,
) -> Option<MemoryTarget> {
    use crate::df::{
        Reg,
        v3::{CCM0, DRAM_BASE, DRAM_LIMIT},
    };
    if sockets == 0 || usize::from(sockets) > MAX_SOCKETS || address >= 1 << 48 {
        return None;
    }
    let f0 = |s, i, off| df(s, i, Reg::new(0, off));
    let m0 = df(0, BROADCAST, Reg::new(1, 0x208));
    let m1 = df(0, BROADCAST, Reg::new(1, 0x20c));
    let component_mask = FABRIC_MASK0::COMPONENT.read(m0);
    let node_mask = FABRIC_MASK0::NODE.read(m0);
    let node_shift = FABRIC_MASK1::NODE_SHIFT.read(m1);
    let socket_shift = node_shift + FABRIC_MASK1::SOCKET_SHIFT.read(m1);
    let socket_mask = FABRIC_MASK1::SOCKET.read(m1) << node_shift;
    if component_mask == 0
        || component_mask & node_mask != 0
        || socket_mask & !node_mask != 0
        || (sockets > 1 && socket_mask == 0)
    {
        return None;
    }
    // Global rules are on a CCM, not the UMC's two local rules.
    let (base, limit) = (0..16u16).find_map(|i| {
        let base = f0(0, CCM0, 0x110 + i * 8);
        let limit = f0(0, CCM0, 0x114 + i * 8);
        rule_contains(address, base, limit).then_some((base, limit))
    })?;
    if DRAM_BASE::ILV_DIE.read(base) != 0 {
        return None;
    }
    let start = DRAM_BASE::ILV_ADDR.read(base) + 8;
    if start > 12 {
        return None;
    }
    let bits = channel_bits(base)?;
    let hash = f0(0, BROADCAST, 0x3f8);
    let bit = |n| ((address >> n) & 1) as u32;
    let mode = DRAM_BASE::ILV_CHAN.read(base);
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
    let mut destination = DRAM_LIMIT::DST.read(limit).checked_add(selected)?;
    if DRAM_BASE::ILV_SOCK.is_set(base) {
        if sockets < 2 || socket_mask == 0 || destination & socket_mask != 0 {
            return None;
        }
        destination |= bit(start + bits) << socket_shift;
    }
    let socket = (destination & socket_mask) >> socket_shift;
    let channel = destination & component_mask;
    if socket >= u32::from(sockets)
        || channel >= 8
        || destination & !(component_mask | socket_mask) != 0
    {
        return None;
    }
    let socket = socket as u8;
    let channel = channel as u8;
    let high_base = f0(socket, channel, 0x118);
    let high = DRAM_BASE::VALID.is_set(high_base)
        && address >= (u64::from(DRAM_BASE::BASE.read(high_base)) << 28);
    let local_base = if high {
        high_base
    } else {
        f0(socket, channel, 0x110)
    };
    let local_limit = f0(socket, channel, if high { 0x11c } else { 0x114 });
    if !rule_contains(address, local_base, local_limit) || DRAM_BASE::ILV_DIE.read(local_base) != 0
    {
        return None;
    }
    let hole = f0(socket, channel, 0x104);
    let offset = if high {
        u64::from(f0(socket, channel, 0x1b4) >> 12) << 28
    } else {
        0
    };
    let normalized = normalize(address, local_base, hole, offset)?;
    // CS base/mask fields below compare normalized address bits 39:8.
    if normalized >= 1 << 40 {
        return None;
    }
    let umc = u32::from(channel) * 0x100000 + 0x50000;
    if UMC_MISC_CFG::DIS_ERR_INJ.is_set(smn(socket, umc + 0x1e0))
        || !UMC_ECC_CTRL::RD_ECC_EN.is_set(smn(socket, umc + 0x14c))
    {
        return None;
    }
    let mut matches = (0..4u8).filter(|&cs| {
        let mask = smn(socket, umc + 0x20 + u32::from(cs / 2) * 4) | 1;
        let base = smn(socket, umc + u32::from(cs) * 4);
        let secondary = smn(socket, umc + 0x10 + u32::from(cs) * 4);
        UMC_BASE::EN.is_set(base)
            && !UMC_BASE::EN.is_set(secondary)
            && ((normalized >> 8) as u32 & !mask) == (base & !mask)
    });
    let cs = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let hash_bit = |off| {
        let h = smn(socket, umc + off);
        if CS_HASH::EN.is_set(h) {
            (((normalized >> 9) as u32 & CS_HASH::ADDR_MASK.read(h)).count_ones() & 1) as u8
        } else {
            0
        }
    };
    Some(MemoryTarget {
        socket,
        channel,
        chip_select: cs ^ hash_bit(0xe8) ^ (hash_bit(0xec) << 1),
    })
}

#[cfg(feature = "rome")]
fn rule_contains(address: u64, base: u32, limit: u32) -> bool {
    use crate::df::v3::{DRAM_BASE, DRAM_LIMIT};
    DRAM_BASE::VALID.is_set(base)
        && address >= u64::from(DRAM_BASE::BASE.read(base)) << 28
        && address < (u64::from(DRAM_LIMIT::LIMIT.read(limit)) + 1) << 28
}

#[cfg(feature = "rome")]
fn normalize(address: u64, base: u32, hole: u32, offset: u64) -> Option<u64> {
    use crate::df::v3::DRAM_BASE;
    let address = if DRAM_BASE::HOLE_EN.is_set(base) {
        let hole_base = u64::from(hole & 0xff00_0000);
        if hole & 1 == 0 || (hole_base..1 << 32).contains(&address) {
            return None;
        }
        if address >= 1 << 32 {
            address.checked_sub((1 << 32) - hole_base)?
        } else {
            address
        }
    } else {
        address
    };
    let address = address.checked_sub(u64::from(DRAM_BASE::BASE.read(base)) << 28)?;
    let start = DRAM_BASE::ILV_ADDR.read(base) + 8;
    if start > 12 {
        return None;
    }
    let width = channel_bits(base)? + u32::from(DRAM_BASE::ILV_SOCK.is_set(base));
    let low = address & ((1 << start) - 1);
    let address = if DRAM_BASE::ILV_CHAN.read(base) >= 0xd {
        let middle = 11u32.checked_sub(start)?;
        low | (((address >> (start + 1)) & ((1 << middle) - 1)) << start)
            | ((address >> (11 + width)) << 11)
    } else {
        low | ((address >> (start + width)) << start)
    };
    offset.checked_add(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_wire_layout() {
        let c = CorrectableMemory::new(
            0x1234_5000,
            MemoryTarget {
                socket: 1,
                channel: 5,
                chip_select: 2,
            },
        );
        assert_eq!(size_of::<Control>(), 4);
        assert_eq!(size_of::<CorrectableMemory>(), 20);
        assert_eq!(
            c.as_bytes(),
            &[
                4, 0, 0, 0, 0, 0x50, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 0x55, 0, 1, 0, 2, 1
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
                socket: 1,
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
        unsafe { m.send_in_smm(&NoDelay, &mut C2pBuffer::new(Control::END), flag_ptr) }.unwrap();
        assert_eq!(*actions.borrow(), [1, 2, 4, 3]);
    }

    #[cfg(feature = "rome")]
    mod mapping {
        use super::*;
        use crate::df::{Reg, v3::CCM0};
        fn smn(_: u8, off: u32) -> u32 {
            match off & 0xfffff {
                0x5014c => 0x673,
                0x501e0 => 0x113,
                0x50000 => 1,
                0x50020 => 0x03ff_fffe,
                0x500e8 | 0x500ec => 0x03ff_fe00,
                _ => 0,
            }
        }
        fn masks(reg: Reg) -> Option<u32> {
            match (reg.func, reg.offset) {
                (1, 0x208) => Some(0x0020_001f),
                (1, 0x20c) => Some(0x0100_0005),
                _ => None,
            }
        }
        fn bench(s: u8, i: u8, r: Reg) -> u32 {
            if let Some(v) = masks(r) {
                return v;
            }
            assert_eq!(s, 0);
            match (i, r.offset) {
                (CCM0 | 3, 0x110) => 0x3b,
                (CCM0 | 3, 0x114) => 0x0020_4000,
                (_, 0x104) => 0xb000_0001,
                (_, 0x3f8) => 0x0066_6600,
                _ => 0,
            }
        }
        #[test]
        fn captured_1p_map_and_gates() {
            assert_eq!(
                memory_target(0x75bf1000, 1, bench, smn),
                Some(MemoryTarget {
                    socket: 0,
                    channel: 3,
                    chip_select: 0
                })
            );
            assert_eq!(normalize(0x75bf1000, 0x3b, 0xb0000001, 0), Some(0xeb7e000));
            assert_eq!(
                memory_target(0x75bf1000, 1, bench, |s, o| if o & 0xfffff == 0x501e0 {
                    0x117
                } else {
                    smn(s, o)
                }),
                None
            );
            assert_eq!(memory_target(0xb0000000, 1, bench, smn), None);
            assert_eq!(memory_target(0, 0, bench, smn), None);
            assert_eq!(memory_target(0, 3, bench, smn), None);
            assert_eq!(
                memory_target(0x75bf1000, 1, bench, |s, o| {
                    if o & 0xfffff == 0x50004 { 1 } else { smn(s, o) }
                }),
                None
            );
        }
        #[test]
        fn two_socket_interleave_reaches_the_correct_endpoint() {
            let df = |_: u8, _: u8, r: Reg| {
                masks(r).unwrap_or(match r.offset {
                    0x110 => 1 | (5 << 2) | (1 << 8),
                    0x114 => 0x1ff000,
                    _ => 0,
                })
            };
            for (address, socket, channel) in [(0x100000, 0, 0), (0x100800, 1, 0), (0x100f00, 1, 7)]
            {
                let target = memory_target(address, 2, df, |s, o| {
                    assert_eq!(s, socket);
                    smn(s, o)
                })
                .unwrap();
                assert_eq!((target.socket, target.channel), (socket, channel));
            }
            assert_eq!(memory_target(0x100800, 1, df, smn), None);
        }
        #[test]
        fn hashed_two_socket_map_and_remote_permission_gate() {
            let df = |_: u8, _: u8, r: Reg| {
                masks(r).unwrap_or(match r.offset {
                    0x110 => 0x13b,
                    0x114 => 0x204000,
                    0x104 => 0xb0000001,
                    0x3f8 => 0x666600,
                    _ => 0,
                })
            };
            for (address, socket) in [(0x75bf1000, 0), (0x75bf1800, 1)] {
                let target = memory_target(address, 2, df, |s, o| {
                    assert_eq!(s, socket);
                    smn(s, o)
                })
                .unwrap();
                assert_eq!(
                    (target.socket, target.channel, target.chip_select),
                    (socket, 3, 0)
                );
            }
            assert_eq!(
                memory_target(0x75bf1800, 2, df, |s, o| {
                    if s == 1 && o & 0xfffff == 0x501e0 {
                        0x117
                    } else {
                        smn(s, o)
                    }
                }),
                None
            );
            assert_eq!(memory_target(0x75bf1000, 2, df, smn).unwrap().socket, 0);
        }

        #[test]
        fn separate_socket_ranges_and_local_high_offset() {
            let boundary = 1u64 << 35;
            let df = |s: u8, i: u8, r: Reg| {
                if let Some(v) = masks(r) {
                    return v;
                }
                if i == CCM0 {
                    return match r.offset {
                        0x110 => 1,
                        0x114 => 0x7f000,
                        0x118 => 0x80001,
                        0x11c => 0xff020,
                        _ => 0,
                    };
                }
                if i == BROADCAST {
                    return 0;
                }
                assert_eq!(s, 1);
                match r.offset {
                    0x118 => 0x80001,
                    0x11c => 0xff020,
                    0x1b4 => 0x10000,
                    _ => 0,
                }
            };
            let target = memory_target(boundary + 0x1000, 2, df, |s, o| {
                assert_eq!(s, 1);
                smn(s, o)
            })
            .unwrap();
            assert_eq!(target.socket, 1);
            assert_eq!(
                normalize(boundary + 0x1000, 0x80001, 0, 1 << 32),
                Some((1 << 32) + 0x1000)
            );
        }
        #[test]
        fn remapped_hole_and_invalid_layouts() {
            assert_eq!(normalize(1 << 32, 3, 0xb0000001, 0), Some(0xb0000000));
            assert_eq!(normalize(0xb0000000, 3, 0xb0000001, 0), None);
            assert_eq!(
                memory_target(
                    0,
                    2,
                    |s, i, r| if r.func == 1 && r.offset == 0x20c {
                        5
                    } else {
                        bench(s, i, r)
                    },
                    smn
                ),
                None
            );
            assert_eq!(
                memory_target(
                    0,
                    1,
                    |s, i, r| if r.func == 0 && r.offset == 0x110 {
                        1 | (1 << 6)
                    } else {
                        bench(s, i, r)
                    },
                    smn
                ),
                None
            );
        }
    }
}
