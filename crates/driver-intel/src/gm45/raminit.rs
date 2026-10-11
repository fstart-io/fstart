//! GM45 DDR3 discovery and early controller programming.
//!
//! JEDEC, ZQ, receive-enable and high-rate read/write calibration, final
//! physical mapping and post-map register setup are implemented. The caller
//! checks sparse DRAM readback before exposing usable memory.
//! Register sequences follow coreboot gm45/raminit.c
//! and raminit_rcomp_calibration.c (DDR3, non-SFF, cold boot only).

use crate::generic::spd::{DDR3, SPD_MEMORY_TYPE, ddr3};
use fstart_core::mmio::{self, MmioReadWrite};
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

mod map;
mod post_map;
mod training;

/// Coreboot GM45 indexes memory clocks by their physical MHz, not transfer rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemClock {
    Mhz333,
    Mhz400,
    Mhz533,
}

impl MemClock {
    pub(super) const fn mhz(self) -> u32 {
        match self {
            Self::Mhz333 => 333,
            Self::Mhz400 => 400,
            Self::Mhz533 => 533,
        }
    }
}

/// DDR3 selection inputs; both channels must agree on the same clock and CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ddr3Timing {
    pub clock: MemClock,
    pub cas: u8,
    pub tck_256ns: u32,
    pub tras: u32,
    pub trp: u32,
    pub trcd: u32,
    pub twr: u32,
    pub trfc: u32,
    pub trd: u32,
    pub trrd: u32,
    pub tfaw: u32,
    pub twl: u32,
}

/// One DIMM per channel: coreboot's GM45 channel programming cannot represent
/// an additional module in slot 1 without a separate topology implementation.
#[derive(Debug, Clone, Copy)]
pub struct Ddr3Channels {
    pub dimms: [Option<ddr3::Ddr3DimmInfo>; 2],
}

/// Read full SPD only for installed slots; reject unknown types and extra
/// slots rather than accepting the wrong topology and silently omitting RAM.
pub fn probe_ddr3<B: SmBus>(
    bus: &mut B,
    addresses: &[u8; 4],
) -> Result<Ddr3Channels, ServiceError> {
    let mut dimms = [None; 2];
    for (slot, &addr) in addresses.iter().enumerate() {
        if addr == 0 {
            continue;
        }
        let mem_type = match bus.read_byte(addr, SPD_MEMORY_TYPE) {
            Ok(mem_type) => mem_type,
            Err(_) => continue, // An unpopulated optional slot does not respond.
        };
        // A responding but unknown SPD is not an empty slot.
        if mem_type != DDR3 {
            return Err(ServiceError::HardwareError);
        }
        if slot & 1 != 0 {
            return Err(ServiceError::HardwareError);
        }
        let spd = ddr3::read_spd(bus, addr)?.ok_or(ServiceError::HardwareError)?;
        dimms[slot / 2] =
            Some(ddr3::decode_dimm_gm45(&spd).map_err(|_| ServiceError::HardwareError)?);
    }
    if dimms.iter().all(Option::is_none) {
        return Err(ServiceError::HardwareError);
    }
    Ok(Ddr3Channels { dimms })
}

fn ceil_div(a: u32, b: u32) -> u32 {
    a / b + u32::from(!a.is_multiple_of(b))
}

/// Find the common speed and derived cycles from GM45 `raminit.c` lines
/// 541..723. FSB is 667/800/1067 MHz; the host bridge DDR3 limit is 800 or
/// 1067 MT/s. Neither of these inputs may be inferred from SPD alone.
pub fn select_ddr3_timing(
    channels: &Ddr3Channels,
    fsb_mhz: u32,
    max_ddr3_mt: u32,
) -> Result<Ddr3Timing, ServiceError> {
    if !matches!(fsb_mhz, 667 | 800 | 1067) || !matches!(max_ddr3_mt, 800 | 1067) {
        return Err(ServiceError::HardwareError);
    }
    let mut mask = !0u32;
    let mut tck_min = 0;
    let mut taa_min = 0;
    let mut tras = 0;
    let mut trp = 0;
    let mut trcd = 0;
    let mut twr = 0;
    let mut density = 0;
    let mut page_size = 0;
    let mut populated = false;
    for dimm in channels.dimms.iter().flatten() {
        ddr3::validate_gm45(dimm).map_err(|_| ServiceError::HardwareError)?;
        // Reject unsupported page-size combinations before indexing the GM45
        // tFAW table; only 1K/2K chip pages are defined in coreboot.
        if !matches!(dimm.page_size, 1024 | 2048) || dimm.tck_min_256ns == 0 {
            return Err(ServiceError::HardwareError);
        }
        populated = true;
        mask &= dimm.cas_latencies;
        tck_min = tck_min.max(dimm.tck_min_256ns);
        taa_min = taa_min.max(dimm.taa_min_256ns);
        tras = tras.max(dimm.tras_256ns);
        trp = trp.max(dimm.trp_256ns);
        trcd = trcd.max(dimm.trcd_256ns);
        twr = twr.max(dimm.twr_256ns);
        density = density.max(dimm.density_code as usize);
        page_size = page_size.max(dimm.page_size);
    }
    if !populated || tck_min == 0 {
        return Err(ServiceError::HardwareError);
    }
    let max_mhz = (max_ddr3_mt / 2).min(fsb_mhz / 2);
    let supported = 256_000 / tck_min;
    for clock in [MemClock::Mhz533, MemClock::Mhz400, MemClock::Mhz333] {
        let mhz = clock.mhz();
        if mhz > max_mhz || mhz > supported {
            continue;
        }
        let tck = 256_000 / mhz;
        let first_cas = ceil_div(taa_min, tck).max(4);
        for cas in first_cas..=18 {
            if mask & (1 << cas) == 0 || cas * tck >= 160 * 32 {
                continue;
            }
            let index = match clock {
                MemClock::Mhz533 => 0,
                MemClock::Mhz400 => 1,
                MemClock::Mhz333 => 2,
            };
            // Coreboot GM45 tRFC/tFAW tables indexed by chip density and
            // chip page size respectively; do not substitute SPD tRFC.
            const TRFC: [[u32; 4]; 3] = [[40, 56, 68, 104], [30, 42, 51, 78], [25, 35, 43, 65]];
            const TFAW: [[u32; 3]; 2] = [[20, 15, 13], [27, 20, 17]];
            let trd = cas
                + match fsb_mhz {
                    667 => 1,
                    800 => 2,
                    1067 => 3 + u32::from(clock == MemClock::Mhz533),
                    _ => unreachable!(),
                };
            let page_kib = page_size / 1024;
            return Ok(Ddr3Timing {
                clock,
                cas: cas as u8,
                tck_256ns: tck,
                tras: ceil_div(tras, tck),
                trp: ceil_div(trp, tck),
                trcd: ceil_div(trcd, tck),
                twr: ceil_div(twr, tck),
                trfc: TRFC[index][density],
                trd,
                trrd: 2 + page_kib * (1 + u32::from(clock == MemClock::Mhz533)),
                tfaw: TFAW[(page_kib - 1) as usize][index],
                twl: if clock == MemClock::Mhz533 { 6 } else { 5 },
            });
        }
    }
    Err(ServiceError::HardwareError)
}

register_bitfields![u32,
    Dcc [INTERLEAVED OFFSET(1) NUMBITS(1) [], NO_CHANXOR OFFSET(10) NUMBITS(1) [],
        COMMAND OFFSET(16) NUMBITS(3) [], EREG OFFSET(21) NUMBITS(2) []]
];

// Named fixed offsets are represented by the MCHBAR overlay; the RCOMP
// lookup window has data-dependent offsets and is accessed through checked
// dynamic register helpers below.
register_structs! {
    PreJedecRegs {
        (0x0000 => _r0: [u8; 0x40]),
        (0x0040 => jedec_gate: MmioReadWrite<u32>),
        (0x0044 => _r0a: [u8; 0x34]),
        (0x0078 => training_read_control: MmioReadWrite<u8>),
        (0x0079 => _r0aa: [u8; 0x187]),
        (0x0200 => dcc: MmioReadWrite<u32, Dcc::Register>),
        (0x0204 => _dcc_pad: [u8; 4]),
        (0x0208 => crossing0: MmioReadWrite<u32>),
        (0x020c => crossing1: MmioReadWrite<u32>),
        (0x0210 => crossing2: MmioReadWrite<u32>),
        (0x0214 => _cross_pad0: [u8; 4]),
        (0x0218 => misc0: MmioReadWrite<u32>),
        (0x021c => _cross_pad1: [u8; 4]),
        (0x0220 => misc1: MmioReadWrite<u32>),
        (0x0224 => misc2: MmioReadWrite<u32>),
        (0x0228 => _cross_pad2: [u8; 8]),
        (0x0230 => jedec_setup: MmioReadWrite<u32>),
        (0x0234 => misc3: MmioReadWrite<u8>),
        (0x0235 => _r0b: [u8; 3]),
        (0x0238 => jedec_channel0: MmioReadWrite<u32>),
        (0x023c => jedec_channel1: MmioReadWrite<u32>),
        (0x0240 => _r0c: [u8; 0x1c0]),
        (0x0400 => rcomp_ctrl: MmioReadWrite<u32>),
        (0x0404 => rcomp_io: MmioReadWrite<u32>),
        (0x0408 => _r1: [u8; 4]),
        (0x040c => rcomp_cfg: MmioReadWrite<u32>),
        (0x0410 => rcomp_io2: MmioReadWrite<u32>),
        (0x0414 => _r2: [u8; 4]),
        (0x0418 => rcomp_cfg2: MmioReadWrite<u32>),
        (0x041c => rcomp_cfg3: MmioReadWrite<u32>),
        (0x0420 => _r3: [u8; 0xb0]),
        (0x04d0 => rcomp_odt: MmioReadWrite<u32>),
        (0x04d4 => _r4: [u8; 0x44]),
        (0x0518 => rcomp_index: MmioReadWrite<u32>),
        (0x051c => _r5: [u8; 0x14]),
        (0x0530 => rcomp_status: MmioReadWrite<u32>),
        (0x0534 => _r6: [u8; 0x5e0]),
        (0x0b14 => quadcore_control: MmioReadWrite<u32>),
        (0x0b18 => _r6a: [u8; 0xe8]),
        (0x0c00 => clkcfg: MmioReadWrite<u32>),
        (0x0c04 => _r7: [u8; 0x0b]),
        (0x0c0f => hpll_vco: MmioReadWrite<u8>),
        (0x0c10 => _r7a: [u8; 0x0c]),
        (0x0c1c => scratchpad: MmioReadWrite<u16>),
        (0x0c1e => _r7b: [u8; 0x2a]),
        (0x0c48 => clk_gate: MmioReadWrite<u32>),
        (0x0c4c => _r8: [u8; 0x14]),
        (0x0c60 => clk_gate2: MmioReadWrite<u16>),
        (0x0c62 => _r9: [u8; 0x2ae]),
        (0x0f10 => igd_gate: MmioReadWrite<u8>),
        (0x0f11 => _r9b: [u8; 0x28d]),
        (0x119e => igd_index: MmioReadWrite<u16>),
        (0x11a0 => _r9c: [u8; 0x20]),
        (0x11c0 => igd_control: MmioReadWrite<u16>),
        (0x11c2 => _r9d: [u8; 0xe2]),
        (0x12a4 => receive_enable0: MmioReadWrite<u32>),
        (0x12a8 => _rec_pad: [u8; 0xfc]),
        (0x13a4 => receive_enable1: MmioReadWrite<u32>),
        (0x13a8 => _rec_pad1: [u8; 0x58]),
        (0x1400 => io0: MmioReadWrite<u32>),
        (0x1404 => _io_pad0: [u8; 8]),
        (0x140c => io1: MmioReadWrite<u32>),
        (0x1410 => _io_pad1: [u8; 4]),
        (0x1414 => io2: MmioReadWrite<u32>),
        (0x1418 => io3: MmioReadWrite<u32>),
        (0x141c => io4: MmioReadWrite<u32>),
        (0x1420 => _io_pad2: [u8; 8]),
        (0x1428 => io5: MmioReadWrite<u32>),
        (0x142c => io6: MmioReadWrite<u32>),
        (0x1430 => _io_pad3: [u8; 4]),
        (0x1434 => dram_ctrl: MmioReadWrite<u32>),
        (0x1438 => _io_pad4: [u8; 8]),
        (0x1440 => io7: MmioReadWrite<u32>),
        (0x1444 => _io_pad5: [u8; 0x4c]),
        (0x1490 => write_io0: MmioReadWrite<u32>),
        (0x1494 => write_io1: MmioReadWrite<u32>),
        (0x1498 => _io_pad6: [u8; 0x58]),
        (0x14f0 => write_pointer0: MmioReadWrite<u32>),
        (0x14f4 => _io_pad7: [u8; 0x9c]),
        (0x1590 => write_io2: MmioReadWrite<u32>),
        (0x1594 => write_io3: MmioReadWrite<u32>),
        (0x1598 => _io_pad8: [u8; 0x58]),
        (0x15f0 => write_pointer1: MmioReadWrite<u32>),
        (0x15f4 => @END),
    }
}

// These eight DDR3 seed pairs share one register shape, but not a uniform
// offset stride: coreboot skips 0x82c/0x830 between the sixth and seventh.
const RCOMP_DDR3_SEEDS: [(usize, u8); 8] = [
    (0x6ac, 0x55),
    (0x6ec, 0x66),
    (0x72c, 0x66),
    (0x76c, 0x66),
    (0x7ac, 0x66),
    (0x7ec, 0x66),
    (0x86c, 0x55),
    (0x8ac, 0x66),
];

register_structs! {
    RcompSeedRegs {
        (0x00 => control: MmioReadWrite<u8>),
        (0x01 => _pad: [u8; 3]),
        (0x04 => value: MmioReadWrite<u8>),
        (0x05 => @END),
    }
}

struct Mchbar {
    base: usize,
}

impl Mchbar {
    fn new(base: u64) -> Self {
        Self {
            base: base as usize,
        }
    }
    fn regs(&self) -> &PreJedecRegs {
        // SAFETY: host-bridge early decode enabled MCHBAR; the overlay ends
        // inside its 16 KiB aperture and contains only aligned registers.
        unsafe { &*(self.base as *const PreJedecRegs) }
    }
    fn post_jedec_channels(&self) -> [&PostJedecChannelRegs; 2] {
        // SAFETY: fixed channel 0/1 register blocks within enabled MCHBAR.
        unsafe {
            [
                &*((self.base + 0x1228) as *const PostJedecChannelRegs),
                &*((self.base + 0x1328) as *const PostJedecChannelRegs),
            ]
        }
    }
    fn rcomp_seed_regs(&self, offset: usize) -> &RcompSeedRegs {
        assert!(RCOMP_DDR3_SEEDS.iter().any(|(seed, _)| *seed == offset));
        // SAFETY: this is one of the eight fixed seed offsets in the enabled
        // MCHBAR aperture; the overlay names the control and value bytes.
        unsafe { &*((self.base + offset) as *const RcompSeedRegs) }
    }
    // Only variable channel/rank/lane or RCOMP table offsets pass through
    // these helpers. All callers bound their indices before accessing MMIO.
    fn read32(&self, offset: usize) -> u32 {
        assert!(offset < 0x4000 && offset.is_multiple_of(4));
        // SAFETY: bounded, aligned offset in the enabled MCHBAR aperture.
        unsafe { mmio::read32((self.base + offset) as *const u32) }
    }
    fn write32(&self, offset: usize, value: u32) {
        assert!(offset < 0x4000 && offset.is_multiple_of(4));
        // SAFETY: bounded, aligned offset in the enabled MCHBAR aperture.
        unsafe { mmio::write32((self.base + offset) as *mut u32, value) }
    }
    fn update32(&self, offset: usize, clear: u32, set: u32) {
        self.write32(offset, (self.read32(offset) & !clear) | set);
    }
    fn update8(&self, offset: usize, clear: u8, set: u8) {
        assert!(offset < 0x4000);
        // SAFETY: bounded offset in the enabled MCHBAR aperture.
        let addr = (self.base + offset) as *mut u8;
        unsafe { mmio::write8(addr, (mmio::read8(addr) & !clear) | set) }
    }
}

register_structs! {
    PostJedecChannelRegs {
        (0x00 => timing6: MmioReadWrite<u32>),
        (0x04 => _pad: [u8; 8]),
        (0x0c => pointer_control: MmioReadWrite<u32>),
        (0x10 => _pad2: [u8; 0x18]),
        (0x28 => control0: MmioReadWrite<u32>),
        (0x2c => control1: MmioReadWrite<u32>),
        (0x30 => @END),
    }
}

register_structs! {
    EpbarVc1Regs {
        (0x0000 => _pad: [u8; 0x2c]),
        (0x002c => interval: MmioReadWrite<u8>),
        (0x002d => _pad2: [u8; 0xb]),
        (0x0038 => slot0: MmioReadWrite<u32>),
        (0x003c => slot1: MmioReadWrite<u32>),
        (0x0040 => @END),
    }
}

fn channel_offset(channel: usize, reg: usize) -> usize {
    assert!(channel < 2);
    assert!((0x1200..0x1300).contains(&reg) && reg.is_multiple_of(4));
    reg + channel * 0x100
}

fn clock_setting(clock: MemClock) -> u32 {
    match clock {
        MemClock::Mhz533 => 6,
        MemClock::Mhz400 => 5,
        MemClock::Mhz333 => 4,
    }
}

/// Coreboot CLKCFG sequence; zero MEMCLK selects the scratchpad fallback.
fn set_memory_clock(mch: &Mchbar, clock: MemClock, fsb_mhz: u32) -> Result<(), ServiceError> {
    let regs = mch.regs();
    let wanted = clock_setting(clock);
    // The FSB1067/DDR3-667 pairing needs its own additional PLL sequence.
    if fsb_mhz == 1067 && clock == MemClock::Mhz333 {
        return Err(ServiceError::HardwareError);
    }
    let mut current = (regs.clkcfg.get() >> 4) & 7;
    if current == 0 {
        current = u32::from(regs.scratchpad.get() & 7);
    }
    if current > 6 {
        return Err(ServiceError::HardwareError);
    }
    regs.clk_gate2.set(regs.clk_gate2.get() & !(1 << 15));
    regs.clk_gate.set(regs.clk_gate.get() & !(1 << 15));
    if current != wanted {
        let reg = regs.clkcfg.get() & !((7 << 4) | (1 << 12));
        regs.clkcfg.set(reg | wanted << 4);
        regs.clkcfg.set(reg | wanted << 4 | (1 << 12));
        regs.clkcfg.set(reg | wanted << 4);
    }
    // The CLKCFG + 0x48 32-bit gate write is part of the frequency sequence;
    // its lower 16 bits include the gate bit cleared above.
    regs.clk_gate.set(regs.clk_gate.get() | (0x3f << 24));
    Ok(())
}

// Coreboot gm45/raminit.c get_gmch_info() CAPID0+4 subtype and render
// fuse decode. GS45 requires an independently established power mode.
fn igd_variant(capid_high: u32) -> Result<usize, ServiceError> {
    const TYPES: [[Option<usize>; 5]; 8] = [
        [None; 5],
        [Some(1), Some(0), None, None, Some(2)],
        [Some(3); 5],
        [None, Some(5), Some(4), None, None],
        [None, Some(7), Some(6), None, None],
        [None; 5],
        [None; 5],
        [Some(8); 5],
    ];
    let variant = ((capid_high >> 10) & 7) as usize;
    let render = (((capid_high >> 18) & 1) << 2 | ((capid_high >> 3) & 3)) as usize;
    let subtype = TYPES[variant]
        .get(render)
        .copied()
        .flatten()
        .ok_or(ServiceError::HardwareError)?;
    // GS45's low-power/performance distinction is board policy, not a fuse
    // decoded here; refuse to guess its table column.
    if subtype == 7 {
        return Err(ServiceError::HardwareError);
    }
    Ok(subtype)
}

fn igd_frequency_codes(vco: u8, subtype: usize) -> Result<(u16, u16, u16), ServiceError> {
    const RENDER: [[u16; 9]; 4] = [
        [13, 13, 14, 13, 11, 13, 11, 10, 13],
        [13, 14, 15, 13, 11, 13, 11, 9, 13],
        [12, 13, 15, 12, 10, 12, 10, 9, 12],
        [11, 12, 14, 11, 9, 11, 9, 8, 11],
    ];
    const SAMPLER: [[u16; 9]; 4] = [
        [12, 12, 13, 12, 9, 12, 9, 8, 12],
        [12, 13, 14, 12, 9, 12, 9, 8, 12],
        [10, 12, 13, 10, 8, 10, 8, 8, 10],
        [10, 10, 12, 10, 7, 10, 7, 6, 10],
    ];
    let vco_index = match vco & 7 {
        4 => 0,
        0 => 1,
        1 => 2,
        2 => 3,
        _ => return Err(ServiceError::HardwareError),
    };
    let render = *RENDER[vco_index]
        .get(subtype)
        .ok_or(ServiceError::HardwareError)?;
    let sampler = SAMPLER[vco_index][subtype];
    Ok((render, sampler, u16::from(subtype != 7)))
}

fn set_igd_memory_frequencies(mch: &Mchbar, capid_high: u32) -> Result<(), ServiceError> {
    let igd = EcamDevice::new(0, 2, 0);
    if igd.read16(0) != 0x8086 {
        return Err(ServiceError::HardwareError);
    }
    let subtype = igd_variant(capid_high)?;
    let (render, sampler, display) = igd_frequency_codes(mch.regs().hpll_vco.get(), subtype)?;
    let regs = mch.regs();
    regs.igd_index.set(0xa800);
    regs.igd_control
        .set((regs.igd_control.get() & !(0xff << 8)) | (1 << 8));
    regs.igd_index.set(0xb800);
    regs.igd_gate.set(regs.igd_gate.get() | (1 << 7));
    let codes = render | (sampler << 8);
    let first = igd.read16(0xf0) & !((0xf << 8) | (1 << 5) | 0xf);
    igd.write16(0xf0, first | codes);
    let next = igd.read16(0xf0) & !((0xf << 8) | 0xf);
    igd.write16(0xf0, next | codes | (1 << 5));
    igd.write16(0xf0, igd.read16(0xf0) & !(1 << 5));
    igd.write16(0xf0, (igd.read16(0xf0) & !(1 << 12)) | (display << 12));
    Ok(())
}

/// Channel rank enable/not-populated bits, 7.8us refresh, DDR3-1067 bit.
fn configure_dram_control(mch: &Mchbar, dimms: &Ddr3Channels, clock: MemClock) {
    for ch in 0..2 {
        let ranks = dimms.dimms[ch].map_or(0, |d| d.ranks);
        let mask = (1u32 << ranks) - 1;
        mch.update32(
            channel_offset(ch, 0x1230),
            (0xf << 24) | (7 << 8),
            (mask << 24) | (2 << 8),
        );
        mch.update32(
            channel_offset(ch, 0x1234),
            0xf << 16,
            ((0xf ^ mask) << 16) | (3 << 11),
        );
        mch.update32(
            channel_offset(ch, 0x1238),
            (0xf << 24) | 1,
            ((0xf ^ mask) << 24) | (1 << 12) | u32::from(clock == MemClock::Mhz533),
        );
    }
}

// The B-step lookup table in coreboot raminit_rcomp_calibration.c. Rows 0..25
// are identical; the remaining rows preserve its complete 64-row domain.
// Conversion A1 differs only at row 29 column 0 and in column 7 (which then
// equals column 6). Index values 7..55 select a 16-row window.
const RCOMP_B_ROWS: [[u8; 8]; 38] = [
    [9, 9, 3, 3, 3, 3, 6, 7],
    [9, 9, 3, 3, 3, 3, 6, 7],
    [10, 10, 3, 3, 3, 3, 7, 8],
    [11, 10, 3, 3, 3, 3, 7, 8],
    [12, 11, 3, 3, 3, 3, 8, 9],
    [13, 11, 3, 3, 3, 3, 9, 9],
    [14, 12, 3, 3, 3, 3, 9, 10],
    [15, 13, 3, 3, 3, 3, 9, 10],
    [16, 14, 3, 3, 3, 3, 9, 11],
    [18, 16, 3, 3, 3, 3, 10, 12],
    [20, 18, 4, 3, 4, 4, 10, 12],
    [22, 22, 4, 4, 4, 4, 11, 12],
    [24, 24, 4, 4, 4, 4, 11, 12],
    [28, 26, 4, 4, 4, 4, 12, 12],
    [32, 28, 5, 4, 5, 5, 12, 12],
    [36, 32, 5, 5, 5, 5, 13, 13],
    [40, 36, 5, 5, 5, 5, 14, 13],
    [43, 40, 5, 5, 5, 5, 15, 14],
    [43, 43, 5, 5, 6, 5, 15, 14],
    [43, 43, 6, 5, 6, 5, 15, 15],
    [43, 43, 6, 5, 6, 6, 15, 15],
    [43, 43, 6, 6, 6, 6, 15, 15],
    [43, 43, 6, 6, 7, 6, 15, 15],
    [43, 43, 7, 6, 7, 6, 15, 15],
    [43, 43, 7, 7, 7, 7, 15, 15],
    [43, 43, 7, 7, 7, 7, 15, 15],
    [43, 43, 7, 7, 7, 7, 15, 15],
    [43, 43, 8, 7, 8, 7, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
    [43, 43, 8, 8, 8, 8, 15, 15],
];

fn rcomp_lookup(conversion_a1: bool, row: usize, col: usize) -> u8 {
    assert!(row < 64 && col < 8);
    let mut entry = if row < 26 {
        [8, 8, 3, 3, 3, 3, 5, 7]
    } else {
        RCOMP_B_ROWS[row - 26]
    };
    if conversion_a1 {
        if row == 29 {
            entry[0] = 10;
        }
        entry[7] = entry[6];
    }
    entry[col]
}

/// Each 16-entry window consists of four dwords with four 6-bit LUT codes.
fn rcomp_words(conversion_a1: bool, index: u8, col: usize) -> Option<[u32; 4]> {
    if !(7..=55).contains(&index) || col >= 8 {
        return None;
    }
    let mut words = [0; 4];
    for (word, value) in words.iter_mut().enumerate() {
        for byte in 0..4 {
            *value |= u32::from(rcomp_lookup(
                conversion_a1,
                usize::from(index - 7) + 4 * word + byte,
                col,
            )) << (8 * byte);
        }
    }
    Some(words)
}

fn rcomp_pulse_once(mch: &Mchbar) {
    let ctrl = &mch.regs().rcomp_ctrl;
    ctrl.set(ctrl.get() | (1 << 3));
    fstart_arch::x86::udelay(10);
    ctrl.set(ctrl.get() & !(1 << 3));
}

fn rcomp_pulse(mch: &Mchbar) -> Result<(), ServiceError> {
    let regs = mch.regs();
    // Coreboot reissues the 10-us pulse until the calibration status is 4.
    // Bound *pulse attempts*, not just status reads, so a stuck controller
    // fails closed without losing the required retry sequence.
    for _ in 0..10_000 {
        rcomp_pulse_once(mch);
        if regs.rcomp_status.get() & 7 == 4 {
            return Ok(());
        }
    }
    Err(ServiceError::HardwareError)
}

fn rcomp_run(mch: &Mchbar) -> Result<(), ServiceError> {
    let regs = mch.regs();
    regs.rcomp_ctrl.set(regs.rcomp_ctrl.get() | 1);
    for _ in 0..100_000 {
        if regs.rcomp_ctrl.get() & 1 == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ServiceError::HardwareError)
}

// Coreboot writes pull-down first at the group's lower address, then
// pull-up at +0x18. `indices` stores pull-up at 0 and pull-down at 1.
const fn rcomp_table_offset(channel: usize, group: usize, pull: usize) -> usize {
    0x680 + channel * 0x1c0 + group * 0x40 + (1 - pull) * 0x18
}

fn initialize_rcomp(mch: &Mchbar, revision: u8) -> Result<(), ServiceError> {
    let regs = mch.regs();
    for (offset, value) in RCOMP_DDR3_SEEDS {
        let seed = mch.rcomp_seed_regs(offset);
        seed.control.set(seed.control.get() & !0x0f);
        seed.value.set(value);
    }
    regs.rcomp_odt
        .set((regs.rcomp_odt.get() & !0x3f) | (2 << 3) | 2);
    regs.rcomp_ctrl.set(regs.rcomp_ctrl.get() | (1 << 2));
    regs.rcomp_cfg2.set(regs.rcomp_cfg2.get() | (1 << 17));
    regs.rcomp_cfg.set(regs.rcomp_cfg.get() & !(1 << 23));
    regs.rcomp_cfg3
        .set(regs.rcomp_cfg3.get() & !((1 << 7) | (1 << 3)));
    regs.rcomp_ctrl.set(regs.rcomp_ctrl.get() | 1);

    let mut indices = [[[None; 2]; 6]; 2];
    let captured = (|| {
        for _ in 0..12 {
            rcomp_pulse(mch)?;
            let ctrl = regs.rcomp_ctrl.get();
            let group = ((ctrl >> 13) & 7) as usize;
            if group > 5 {
                break;
            }
            let channel = ((ctrl >> 12) & 1) as usize;
            let index = regs.rcomp_index.get();
            indices[channel][group] = [
                Some(((index >> 24) & 0x7f) as u8),
                Some(((index >> 16) & 0x7f) as u8),
            ];
        }
        // Cleanup advances the engine once; unlike capture, it has no
        // status-4 completion requirement (coreboot RCOMP calibration).
        rcomp_pulse_once(mch);
        Ok::<_, ServiceError>(())
    })();
    // Coreboot stops calibration before validating and writing lookup tables.
    // Also clear the enable bit when a bounded pulse times out.
    regs.rcomp_ctrl.set(regs.rcomp_ctrl.get() & !(1 << 2));
    captured?;
    // Validate every index before programming any table: channel B exposes
    // only two table groups, but coreboot still validates all six readings.
    for channel in &indices {
        for group in channel {
            for &index in group {
                if !matches!(index, Some(7..=55)) {
                    return Err(ServiceError::HardwareError);
                }
            }
        }
    }
    for channel in 0..2 {
        for group in 0..(if channel == 0 { 6 } else { 2 }) {
            for pull in (0..2).rev() {
                let index = indices[channel][group][pull].ok_or(ServiceError::HardwareError)?;
                let col = [[0, 1], [2, 3], [4, 5], [4, 5], [6, 7], [6, 7]][group][pull];
                let words =
                    rcomp_words(revision == 9, index, col).ok_or(ServiceError::HardwareError)?;
                // Group stride 0x40; channel B starts at 0x840.
                let base = rcomp_table_offset(channel, group, pull);
                for (i, word) in words.iter().enumerate() {
                    mch.write32(base + i * 4, *word);
                }
            }
        }
    }
    rcomp_run(mch)?;
    regs.rcomp_cfg.set(regs.rcomp_cfg.get() | (1 << 19));
    rcomp_run(mch)?;
    regs.rcomp_cfg
        .set((regs.rcomp_cfg.get() & !(1 << 19)) | (1 << 23));
    regs.rcomp_cfg2.set(regs.rcomp_cfg2.get() & !(1 << 17));
    regs.rcomp_cfg3
        .set(regs.rcomp_cfg3.get() | (1 << 7) | (1 << 3));
    regs.rcomp_ctrl.set(regs.rcomp_ctrl.get() | (1 << 1));
    Ok(())
}

/// Reject timings that would silently truncate in GM45's DDR3 fields.
fn validate_register_timings(t: &Ddr3Timing) -> Result<(), ServiceError> {
    let cas = u32::from(t.cas);
    let cas_max = if t.clock == MemClock::Mhz533 { 10 } else { 9 };
    if !(5..=cas_max).contains(&cas)
        || t.tras > 31
        || !(2..=9).contains(&t.trp)
        || !(2..=9).contains(&t.trcd)
        || !(2..=9).contains(&t.trrd)
        || t.trfc > 255
        || t.tfaw > 31
        || t.twl < 2
        || t.twl > 9
        || t.twl + 4 + t.twr > 31
        || t.twl + 8 > 31
        || cas + 6 > 15
        || 2 + cas > 15
        || t.trd > 31
    {
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

fn program_timings(mch: &Mchbar, t: &Ddr3Timing) {
    let cas = u32::from(t.cas);
    let is_1067 = t.clock == MemClock::Mhz533;
    let clock_index = match t.clock {
        MemClock::Mhz333 => 0,
        MemClock::Mhz400 => 1,
        MemClock::Mhz533 => 2,
    };
    // coreboot dram_program_timings(), DDR3 path, CxDRT0..6.
    const DRT4: [[u32; 3]; 4] = [
        [0x07, 0x0a, 0x0d],
        [0x3a, 0x46, 0x5d],
        [0x0c, 0x0e, 0x18],
        [0x21, 0x28, 0x35],
    ];
    for ch in 0..2 {
        let offset = |reg| channel_offset(ch, reg);
        mch.update32(
            offset(0x1210),
            (0x1f << 26) | (0x1f << 20) | (7 << 15) | (0xf << 10) | (7 << 5) | 7,
            ((t.twl + 4 + t.twr) << 26)
                | ((t.twl + 8) << 20)
                | ((if is_1067 { 10 - cas } else { 9 - cas }) << 15)
                | ((cas - if is_1067 { 4 } else { 3 }) << 10)
                | (3 << 5)
                | 1,
        );
        mch.update32(
            offset(0x1214),
            (3 << 28) | (0x1f << 21) | (7 << 10) | (7 << 5) | 7,
            (1 << 28) | (t.tras << 21) | ((t.trrd - 2) << 10) | ((t.trcd - 2) << 5) | (t.trp - 2),
        );
        mch.update32(
            offset(0x1218),
            (0x1f << 17) | (7 << 12) | (0xf << 6) | 0x1f,
            (t.tfaw << 17)
                | ((if is_1067 { 3 } else { 2 }) << 12)
                | ((if is_1067 { 0xc } else { 9 }) << 6)
                | 0x13,
        );
        mch.update32(
            offset(0x121c),
            (3 << 28) | (3 << 26) | (7 << 23) | (0xff << 13) | 7,
            (3 << 28) | ((cas - 3) << 23) | (t.trfc << 13) | (t.twl - 2),
        );
        let timing4 = (DRT4[0][clock_index] << 27)
            | (DRT4[1][clock_index] << 17)
            | (DRT4[2][clock_index] << 10)
            | DRT4[3][clock_index];
        mch.update32(
            offset(0x1220),
            (0x1f << 27) | (0x3ff << 17) | (0x3f << 10) | 0x1ff,
            timing4,
        );
        let timing5 = ((if is_1067 {
            8
        } else {
            mch.read32(offset(0x1224)) >> 28 & 0xf
        }) << 28)
            | ((cas + 6) << 22)
            | (0x190 << 12)
            | ((cas - 2) << 4)
            | (1 << 2);
        mch.update32(
            offset(0x1224),
            (0xf << 28) | (0xf << 22) | (0x1ff << 12) | (0xf << 4) | 0xf,
            timing5,
        );
        mch.update32(
            offset(0x1228),
            (0xffff << 16) | (1 << 2),
            (0x066a << 16) | (1 << 2),
        );
    }
}

fn program_banks(mch: &Mchbar, dimms: &Ddr3Channels) {
    for ch in 0..2 {
        let ranks = dimms.dimms[ch].map_or(0, |d| d.ranks);
        mch.update32(
            channel_offset(ch, 0x1214),
            1 << 15,
            u32::from(ranks > 0) << 15,
        );
        let banks = (0..usize::from(ranks)).fold(0u32, |bits, rank| bits | (1 << (16 + 3 * rank)));
        mch.update32(channel_offset(ch, 0x1208), 0x07ff_0000, banks);
    }
}

fn program_odt(mch: &Mchbar, t: &Ddr3Timing) {
    let cas = u32::from(t.cas);
    let is_1067 = t.clock == MemClock::Mhz533;
    let high = (3 << 29)
        | (2 << 20)
        | ((cas - 3) << 16)
        | (7 << 12)
        | ((if is_1067 { 13 - cas } else { 12 - cas }) << 8)
        | ((if is_1067 { 1 + cas } else { 2 + cas }) << 4)
        | 7;
    let low = (2 << 28)
        | (2 << 22)
        | (2 << 12)
        | (2 << 4)
        | match t.clock {
            MemClock::Mhz333 => 0,
            MemClock::Mhz400 => 2,
            MemClock::Mhz533 => 5,
        };
    for ch in 0..2 {
        mch.update32(
            channel_offset(ch, 0x124c),
            (3 << 29) | (3 << 20) | (7 << 16) | (0xf << 12) | (0xf << 8) | (0xf << 4) | 0xf,
            high,
        );
        mch.update32(
            channel_offset(ch, 0x1248),
            (7 << 28) | (3 << 22) | (7 << 12) | (7 << 4) | 7,
            low,
        );
    }
}

fn misc_settings(mch: &Mchbar, timing: &Ddr3Timing) {
    let regs = mch.regs();
    for ch in 0..2 {
        mch.update32(channel_offset(ch, 0x1260), (1 << 24) | 0x1f, timing.trd);
        mch.update8(0x1268 + ch * 0x100, 0xf, timing.twl as u8);
        mch.update8(0x12a0 + ch * 0x100, 0xf, 0xa);
    }
    regs.misc0.set(
        (regs.misc0.get() & !((7 << 29) | (7 << 25) | (3 << 22) | (3 << 10)))
            | (4 << 29)
            | (3 << 25)
            | (1 << 10),
    );
    regs.misc1
        .set((regs.misc1.get() & !(7 << 16)) | (1 << 21) | (1 << 16));
    regs.misc2.set((regs.misc2.get() & !(7 << 8)) | (3 << 8));
    regs.misc3.set(regs.misc3.get() | (1 << 3));
}

fn crossing_values(fsb_mhz: u32, clock: MemClock) -> Result<(u32, u32, u32, u32), ServiceError> {
    // raminit.c clock_crossing_setup(): [0x20c, 0x208, 0x210, Cx1258].
    let values = match (fsb_mhz, clock) {
        (1067, MemClock::Mhz533) => (0x0018_0006, 0x0081_0060, 0, 0x4010_0401),
        (1067, MemClock::Mhz400) => (0x0000_001c, 0x0003_00e0, 0, 0x1004_0220),
        (800, MemClock::Mhz400) => (0x0030_000c, 0x0003_00c0, 0, 0x4010_0401),
        (800, MemClock::Mhz333) => (0x0060_001c, 0x0003_0c00, 0x380, 0x0008_0201),
        (667, MemClock::Mhz333) => (0x0030_000c, 0x0003_00c0, 0, 0x4010_0401),
        _ => return Err(ServiceError::HardwareError),
    };
    Ok(values)
}

fn clock_crossing_setup(
    mch: &Mchbar,
    fsb_mhz: u32,
    clock: MemClock,
    dimms: &Ddr3Channels,
) -> Result<(), ServiceError> {
    let (cross1, cross0, cross2, channel_default) = crossing_values(fsb_mhz, clock)?;
    let regs = mch.regs();
    regs.crossing0.set(cross0);
    regs.crossing1.set(cross1);
    if fsb_mhz == 800 && clock == MemClock::Mhz333 {
        regs.crossing2.set(cross2);
    }
    for (ch, dimm) in dimms.dimms.iter().enumerate() {
        // Coreboot adds 0xa to SPD raw card 5 to obtain internal card F.
        let card_f = dimm.is_some_and(|d| d.raw_card == 5);
        let value = if fsb_mhz == 1067 && clock == MemClock::Mhz400 && card_f {
            0x0804_0120
        } else {
            channel_default
        };
        mch.write32(channel_offset(ch, 0x1258), value);
        mch.write32(channel_offset(ch, 0x125c), 0);
    }
    Ok(())
}

fn vc1_program_timings(epbar: u64, fsb_mhz: u32) -> Result<(), ServiceError> {
    let (interval, slots) = match fsb_mhz {
        1067 => (0x1a, 0x0138_0138),
        800 => (0x14, 0x00f0_00f0),
        667 => (0x10, 0x00c0_00c0),
        _ => return Err(ServiceError::HardwareError),
    };
    // SAFETY: early host-bridge setup enabled EPBAR; all fields fit its aperture.
    let regs = unsafe { &*(epbar as *const EpbarVc1Regs) };
    regs.interval.set(interval);
    regs.slot0.set(slots);
    regs.slot1.set(slots);
    Ok(())
}

fn powerup_ddr3(mch: &Mchbar) {
    let regs = mch.regs();
    fstart_arch::x86::udelay(200);
    regs.clkcfg
        .set((regs.clkcfg.get() & !((3 << 21) | (1 << 3))) | (3 << 21));
    regs.dram_ctrl.set(regs.dram_ctrl.get() | (1 << 10));
    fstart_arch::x86::udelay(1);
    regs.dram_ctrl.set(regs.dram_ctrl.get() | (1 << 6));
    fstart_arch::x86::udelay(1);
    regs.dram_ctrl
        .set((regs.dram_ctrl.get() | (1 << 9)) & !(1 << 10));
    fstart_arch::x86::udelay(500);
}

// Coreboot gm45/raminit.c DDR3 memory-IO setup. The read/write lane and
// channel offsets are computed; all singleton registers use the overlay.
fn io_channel_offset(channel: usize, reg: usize) -> usize {
    assert!(channel < 2 && (0x1470..=0x14cc).contains(&reg) && reg.is_multiple_of(4));
    reg + channel * 0x100
}

fn clock_mux_codes(clock: MemClock, card_f: bool, mixed: bool) -> [u32; 8] {
    let fast = clock == MemClock::Mhz533;
    let codes = [
        if fast && !card_f { 3 } else { 2 },
        if !fast && !card_f { 2 } else { 3 },
        2,
        if card_f { 3 } else { 2 },
        u32::from(fast && !card_f),
        u32::from(fast || card_f),
        0,
        u32::from(card_f),
    ];
    codes.map(|code| (code + if mixed { 4 } else { 0 }) << 11)
}

fn write_io_values(clock: MemClock, revision: u8, channel: usize, card_f: bool) -> [u32; 4] {
    const SLOW: [[[[u32; 4]; 2]; 2]; 2] = [
        [
            [
                [0xa3255008, 0x26888209, 0x26288208, 0x6188040f],
                [0x7524240b, 0xa5255608, 0x232b8508, 0x5528040f],
            ],
            [
                [0xa6255308, 0x26888209, 0x212b7508, 0x6188040f],
                [0x7524240b, 0xa6255708, 0x132b7508, 0x5528040f],
            ],
        ],
        [
            [
                [0xc5257208, 0x26888209, 0x26288208, 0x6188040f],
                [0x7524240b, 0xc5257608, 0x232b8508, 0x5528040f],
            ],
            [
                [0xb6256308, 0x26888209, 0x212b7508, 0x6188040f],
                [0x7524240b, 0xb6256708, 0x132b7508, 0x5528040f],
            ],
        ],
    ];
    const FAST: [[[u32; 4]; 2]; 2] = [
        [
            [0xb2254708, 0x002b7408, 0x132b8008, 0x7228060f],
            [0xb0255008, 0xa4254108, 0x4528b409, 0x9428230f],
        ],
        [
            [0xa4254208, 0x022b6108, 0x132b8208, 0x9228210f],
            [0x6024140b, 0x92244408, 0x252ba409, 0x9328360c],
        ],
    ];
    let card = usize::from(card_f);
    match clock {
        MemClock::Mhz333 => SLOW[usize::from(revision == 9)][0][card],
        MemClock::Mhz400 => SLOW[usize::from(revision == 9)][1][card],
        MemClock::Mhz533 => FAST[channel][card],
    }
}

fn initialize_ddr3_io(mch: &Mchbar, dimms: &Ddr3Channels, clock: MemClock, revision: u8) {
    let regs = mch.regs();
    regs.io0
        .set((regs.io0.get() & !(3 << 13)) | (1 << 9) | (1 << 13));
    let rate = match clock {
        MemClock::Mhz333 => (9, 2, 10, 2, 12, 1, 4),
        MemClock::Mhz400 => (7, 3, 7, 3, 10, 2, 3),
        MemClock::Mhz533 => (8, 4, 4, 4, 7, 2, 1),
    };
    regs.io1.set(
        (regs.io1.get()
            & !(0xff | (1 << 11) | (1 << 12) | (1 << 16) | (1 << 18) | (1 << 27) | (0xf << 28)))
            | (1 << 7)
            | (1 << 11)
            | (1 << 16)
            | (rate.0 << 28),
    );
    regs.io7.set(regs.io7.get() & !1);
    regs.io2.set(
        (regs.io2.get() & !((1 << 20) | (7 << 11) | (0xf << 24) | (0xf << 16)))
            | (3 << 11)
            | (rate.1 << 24)
            | (rate.2 << 16),
    );
    for reg in [&regs.io3, &regs.io4] {
        reg.set(reg.get() & !((1 << 3) | (1 << 11) | (1 << 19) | (1 << 27)));
    }
    regs.io5.set(regs.io5.get() | (1 << 14));
    regs.io6.set(
        (regs.io6.get() & !((0xf << 8) | (7 << 20) | 0xf | (0xf << 24)))
            | (3 << 20)
            | (5 << 24)
            | (rate.3 << 8)
            | rate.4,
    );
    regs.rcomp_ctrl
        .set((regs.rcomp_ctrl.get() & !((3 << 4) | (3 << 16) | (3 << 30))) | (2 << 4) | (2 << 16));
    regs.rcomp_io.set(regs.rcomp_io.get() & !(0xf << 20));
    regs.rcomp_cfg.set(regs.rcomp_cfg.get() & !(1 << 6));
    regs.rcomp_io2
        .set((regs.rcomp_io2.get() & !(7 << 28)) | (2 << 28));
    regs.rcomp_cfg3.set((regs.rcomp_cfg3.get() & !0x77) | 0x11);

    let card_f = dimms.dimms.map(|d| d.is_some_and(|d| d.raw_card == 5));
    for ch in 0..2 {
        if dimms.dimms[ch].is_none() {
            continue;
        }
        let mixed = ch == 1 && (dimms.dimms[0].is_none() || card_f[0] != card_f[1]);
        let mux = clock_mux_codes(clock, card_f[ch], mixed);
        for (lane, code) in mux.iter().enumerate() {
            let offset = io_channel_offset(ch, 0x14b0 + lane * 4);
            mch.update32(offset, 7 << 11, *code);
        }
    }
    for ch in 0..2 {
        if dimms.dimms[ch].is_none() {
            continue;
        }
        // Coreboot skips channel B's write-lane table when both populated
        // channels use the same card type (at every supported clock).
        if ch == 1 && dimms.dimms[0].is_some() && card_f[0] == card_f[1] {
            continue;
        }
        let values = write_io_values(clock, revision, ch, card_f[ch]);
        for (lane, value) in values.iter().enumerate() {
            mch.write32(io_channel_offset(ch, 0x147c - lane * 4), *value);
        }
    }
    regs.write_io0.set(0x00e7_0067);
    regs.write_io1.set(0x000d_8000);
    regs.write_io2.set(0x00e7_0067);
    regs.write_io3.set(0x000d_8000);
    for ch in 0..2 {
        if dimms.dimms[ch].is_none() {
            continue;
        }
        for lane in (0..8).rev() {
            let offset = io_channel_offset(ch, 0x14b0 + lane * 4);
            mch.update32(
                offset,
                (3 << 25) | (1 << 8) | (7 << 16) | (0xf << 20) | (1 << 27),
                (1 << 27) | (rate.5 << 16) | (rate.6 << 20),
            );
        }
    }
}

// CxDRBy stores two rank boundaries (32 MiB units) per dword. The temporary
// map must be sequential even if final channel mode would be interleaved.
fn dummy_rank_map(dimms: &Ddr3Channels) -> ([[u32; 2]; 2], [u32; 2], u32) {
    let mut bounds = [[0; 2]; 2];
    let mut pages = [0; 2];
    let mut base_mb = 0;
    for ch in 0..2 {
        let ranks = usize::from(dimms.dimms[ch].map_or(0, |d| d.ranks));
        assert!(ranks <= 2);
        for rank in 0..4 {
            if rank < ranks {
                base_mb += 128;
                pages[ch] |= 2 << (rank * 4); // 4 KiB page: log2(4096) - 10
            }
            bounds[ch][rank / 2] |= (base_mb >> 5) << ((rank % 2) * 16);
        }
    }
    (bounds, pages, base_mb)
}

fn prejedec_memory_map(mch: &Mchbar, dimms: &Ddr3Channels) {
    let (bounds, pages, total_mb) = dummy_rank_map(dimms);
    for ch in 0..2 {
        for (pair, value) in bounds[ch].iter().enumerate() {
            mch.write32(channel_offset(ch, 0x1200 + pair * 4), *value);
        }
        mch.update32(channel_offset(ch, 0x1208), 0xffff, pages[ch]);
    }
    // 2048 MiB PCI MMIO default, no ME allocation and at most four 128 MiB
    // dummy ranks: no low-memory clipping or remap is possible here.
    // SAFETY: the caller enabled ECAM before memory initialization.
    let hb = unsafe { EcamDevice::new(0, 0, 0).regs::<super::Gm45HostBridgePciConfig>() };
    hb.tom.set((total_mb >> 7) as u16);
    hb.tolud.set((total_mb << 4) as u16);
    hb.touud.set(total_mb as u16);
    hb.remap_base.set(0x3ff);
    hb.remap_limit.set(0);
    let regs = mch.regs();
    regs.dcc
        .modify(Dcc::INTERLEAVED.val(0) + Dcc::NO_CHANXOR.val(1));
}

// gm45/raminit.c jedec_init_ddr3(): command bits in DCC, DRAM address
// bits in the read address (not a write to DRAM). The second MR0 clears DLL
// reset. The pre-JEDEC map places each populated rank at a 128 MiB boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JedecCommand {
    selector: u32,
    address_bits: u32,
}

fn ddr3_jedec_commands(t: &Ddr3Timing) -> Result<[JedecCommand; 5], ServiceError> {
    const WR_LUT: [u32; 8] = [1, 2, 3, 4, 5, 5, 6, 6];
    let wr = *WR_LUT
        .get(
            usize::try_from(t.twr)
                .map_err(|_| ServiceError::HardwareError)?
                .checked_sub(5)
                .ok_or(ServiceError::HardwareError)?,
        )
        .ok_or(ServiceError::HardwareError)?;
    if !(4..=11).contains(&t.cas) || !(5..=12).contains(&t.twl) {
        return Err(ServiceError::HardwareError);
    }
    let mr0 = (wr << 12) | ((u32::from(t.cas) - 4) << 7) | (1 << 6);
    Ok([
        JedecCommand {
            selector: (4 << 16) | (1 << 21),
            address_bits: (t.twl - 5) << 6,
        },
        JedecCommand {
            selector: (4 << 16) | (2 << 21),
            address_bits: 0,
        },
        JedecCommand {
            selector: 4 << 16,
            address_bits: (1 << 9) | (1 << 4),
        },
        JedecCommand {
            selector: 3 << 16,
            address_bits: mr0 | (1 << 11),
        },
        JedecCommand {
            selector: 3 << 16,
            address_bits: mr0,
        },
    ])
}

fn jedec_rank_bases(dimms: &Ddr3Channels) -> Result<[[Option<usize>; 2]; 2], ServiceError> {
    let mut bases = [[None; 2]; 2];
    let mut next = 0;
    for (ch, dimm) in dimms.dimms.iter().enumerate() {
        let ranks = dimm.map_or(0, |d| d.ranks);
        if ranks > 2 {
            return Err(ServiceError::HardwareError);
        }
        for rank in 0..usize::from(ranks) {
            bases[ch][rank] = Some(next);
            next += 128 << 20;
        }
    }
    if next == 0 {
        return Err(ServiceError::HardwareError);
    }
    Ok(bases)
}

fn jedec_ddr3(
    mch: &Mchbar,
    hb: &super::Gm45HostBridgePciConfig,
    bases: &[[Option<usize>; 2]; 2],
    commands: &[JedecCommand; 5],
) {
    let regs = mch.regs();
    regs.jedec_gate.set(regs.jedec_gate.get() | (1 << 1));
    regs.jedec_setup.set(regs.jedec_setup.get() | (3 << 1));
    regs.jedec_channel0
        .set(regs.jedec_channel0.get() | (3 << 24));
    regs.jedec_channel1
        .set(regs.jedec_channel1.get() | (3 << 24));
    for pointer in [&regs.write_pointer0, &regs.write_pointer1] {
        pointer.set(pointer.get() | (1 << 9));
    }
    regs.dcc.modify(Dcc::COMMAND.val(1)); // NOP, preserve EREG select.
    hb.jedec_control.set(hb.jedec_control.get() & !(1 << 2));
    hb.jedec_control.set(hb.jedec_control.get() | (1 << 2));
    fstart_arch::x86::udelay(2);
    for channel in bases {
        for &base in channel.iter().flatten() {
            for command in commands {
                regs.dcc
                    .set((regs.dcc.get() & !((7 << 16) | (3 << 21))) | command.selector);
                // SAFETY: the pre-JEDEC map decodes each dummy 128 MiB rank.
                // Rank 0 is physical address zero: Rust volatile reads require
                // a non-null pointer, so issue the command read in assembly.
                unsafe {
                    core::arch::asm!(
                        "mov {value:e}, dword ptr [{address}]",
                        address = in(reg) (base | command.address_bits as usize),
                        value = out(reg) _,
                        options(nostack, preserves_flags),
                    );
                }
            }
        }
    }
}

fn post_jedec_ddr3(mch: &Mchbar, hb: &super::Gm45HostBridgePciConfig, quadcore: bool) {
    let regs = mch.regs();
    regs.jedec_gate.set(regs.jedec_gate.get() & !(1 << 1));
    regs.jedec_setup.set(regs.jedec_setup.get() & !(3 << 1));
    regs.jedec_setup.set(regs.jedec_setup.get() | (1 << 15));
    regs.jedec_setup.set(regs.jedec_setup.get() & !(1 << 19));
    let [ch0, ch1] = mch.post_jedec_channels();
    ch0.control0.set(0x6c4);
    ch1.control0.set(0x6c4);
    ch0.control1.set(0x871a_066d);
    ch1.control1.set(0x871a_066d);
    regs.jedec_channel0
        .set(regs.jedec_channel0.get() | (1 << 26));
    regs.jedec_channel0
        .set(regs.jedec_channel0.get() & !(3 << 24));
    regs.jedec_channel0
        .set(regs.jedec_channel0.get() | (1 << 23));
    for shift in [20, 17, 14, 11, 8] {
        let value = if shift == 20 { 3 } else { 6 };
        regs.jedec_channel0
            .set((regs.jedec_channel0.get() & !(7 << shift)) | (value << shift));
    }
    regs.jedec_channel1
        .set(regs.jedec_channel1.get() & !(3 << 24));
    regs.jedec_channel1
        .set(regs.jedec_channel1.get() & !(1 << 23));
    for shift in [20, 17, 14, 11, 8] {
        let value = if shift == 20 { 3 } else { 6 };
        regs.jedec_channel1
            .set((regs.jedec_channel1.get() & !(7 << shift)) | (value << shift));
    }
    if quadcore {
        regs.quadcore_control
            .set(regs.quadcore_control.get() | (0xbfbf << 16));
    }
    regs.dcc.set(regs.dcc.get() | (7 << 16) | (1 << 19));
    hb.jedec_control.set(hb.jedec_control.get() | (1 << 2));
    hb.jedec_control.set(hb.jedec_control.get() & !(1 << 2));
    // DDR3 ZQ: wait 2 us, select mode 5, pulse timing6 bit 3 for 1 us,
    // then return to normal command mode 7 (coreboot raminit.c).
    fstart_arch::x86::udelay(2);
    regs.dcc.modify(Dcc::COMMAND.val(5));
    for channel in mch.post_jedec_channels() {
        channel.timing6.set(channel.timing6.get() | (1 << 3));
    }
    fstart_arch::x86::udelay(1);
    for channel in mch.post_jedec_channels() {
        channel.timing6.set(channel.timing6.get() & !(1 << 3));
    }
    regs.dcc.set(regs.dcc.get() | (7 << 16));
}

// coreboot raminit_receive_enable_calibration.c: C is channel-wide after the
// search, while PH/T/P and the group offset PRE live in CxRECy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecTiming {
    c: i32,
    pre: i32,
    ph: i32,
    t: i32,
    p: i32,
    t_bound: i32,
    p_bound: i32,
}

impl RecTiming {
    fn new(timing: &Ddr3Timing) -> Self {
        Self {
            c: i32::from(timing.cas) + 1,
            pre: 0,
            ph: 0,
            t: 0,
            p: 0,
            t_bound: if timing.clock == MemClock::Mhz533 {
                9
            } else {
                12
            },
            p_bound: if timing.clock == MemClock::Mhz533 {
                8
            } else {
                1
            },
        }
    }

    fn normalize(&mut self) -> Result<(), ServiceError> {
        // A search may traverse a negative quarter step or a complete clock.
        // Keep the values signed until every carry has been resolved.
        self.t += self.p.div_euclid(self.p_bound);
        self.p = self.p.rem_euclid(self.p_bound);
        let halves = self.t.div_euclid(self.t_bound);
        self.t = self.t.rem_euclid(self.t_bound);
        self.ph += 2 * halves;
        self.c += self.ph.div_euclid(4);
        self.ph = self.ph.rem_euclid(4);
        if !(0..16).contains(&self.c) || !(0..=3).contains(&self.pre) || self.t > 15 || self.p > 7 {
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }

    fn quarter_step(&mut self) {
        self.t += self.t_bound / 2;
        self.p += (self.t_bound & 1) * (self.p_bound / 2);
    }

    fn quarter_backstep(&mut self) {
        self.t -= self.t_bound / 2;
        self.p -= (self.t_bound & 1) * (self.p_bound / 2);
    }

    fn fields(&self) -> u32 {
        ((self.t as u32) << 28)
            | ((self.p as u32) << 24)
            | ((self.ph as u32) << 22)
            | ((self.pre as u32) << 20)
    }
}

const REC_FIELDS: u32 = (0xf << 28) | (7 << 24) | (3 << 22) | (3 << 20);
const REC_LANES: [[[usize; 2]; 4]; 2] = [
    [[0, 1], [2, 3], [4, 5], [6, 7]],
    [[0, 2], [1, 3], [4, 6], [5, 7]],
];
const REC_PROBE_LANES: [[[usize; 2]; 4]; 2] = [
    [[0, 1], [2, 3], [4, 5], [6, 7]],
    [[0, 0], [3, 3], [6, 6], [5, 5]],
];

fn rec_group_offset(ch: usize, group: usize) -> usize {
    assert!(ch < 2 && group < 4);
    io_channel_offset(ch, 0x14a0 + (3 - group) * 4)
}

fn rec_lane_offset(ch: usize, lane: usize) -> usize {
    assert!(ch < 2 && lane < 8);
    io_channel_offset(ch, 0x14b0 + (7 - lane) * 4)
}

fn program_rec_timing(
    mch: &Mchbar,
    ch: usize,
    group: usize,
    timing: &mut RecTiming,
) -> Result<(), ServiceError> {
    timing.normalize()?;
    mch.update32(channel_offset(ch, 0x121c), 0xf << 7, (timing.c as u32) << 7);
    mch.update32(rec_group_offset(ch, group), REC_FIELDS, timing.fields());
    Ok(())
}

fn read_dqs_level(mch: &Mchbar, ch: usize, lane: usize, rank0: usize) -> bool {
    let pointer = if ch == 0 {
        &mch.regs().write_pointer0
    } else {
        &mch.regs().write_pointer1
    };
    pointer.set(pointer.get() & !(1 << 9));
    pointer.set(pointer.get() | (1 << 9));
    // The temporary rank map places the first populated rank at address 0.
    // Use a hardware read in assembly; Rust's volatile read requires non-null.
    unsafe {
        core::arch::asm!(
            "mov {value:e}, dword ptr [{address}]",
            address = in(reg) rank0,
            value = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
    mch.read32(rec_lane_offset(ch, lane)) & (1 << 30) != 0
}

#[derive(Clone, Copy)]
enum RecProbe {
    AnyHigh,
    BothLow,
    AnyLow,
}

fn seek_rec<S>(
    mch: &Mchbar,
    ch: usize,
    group: usize,
    rank0: usize,
    lanes: [usize; 2],
    timing: &mut RecTiming,
    probe: RecProbe,
    step: S,
) -> Result<(), ServiceError>
where
    S: Fn(&mut RecTiming),
{
    // Also bounded by the representable C range, but a stuck DQS signal
    // must never turn calibration into an unbounded polling loop.
    for _ in 0..256 {
        // Preserve coreboot's short-circuit reads. Each lane read resets
        // the pointer and triggers exactly one rank-zero sampling read.
        let first = read_dqs_level(mch, ch, lanes[0], rank0);
        let searching = match probe {
            RecProbe::AnyHigh => first || read_dqs_level(mch, ch, lanes[1], rank0),
            RecProbe::BothLow => !first && !read_dqs_level(mch, ch, lanes[1], rank0),
            RecProbe::AnyLow => !first || !read_dqs_level(mch, ch, lanes[1], rank0),
        };
        if !searching {
            return Ok(());
        }
        step(timing);
        program_rec_timing(mch, ch, group, timing)?;
    }
    Err(ServiceError::HardwareError)
}

fn rec_edge(
    mch: &Mchbar,
    ch: usize,
    group: usize,
    rank0: usize,
    lanes: [usize; 2],
    timing: &mut RecTiming,
) -> Result<(), ServiceError> {
    timing.t += 2;
    program_rec_timing(mch, ch, group, timing)?;
    seek_rec(
        mch,
        ch,
        group,
        rank0,
        lanes,
        timing,
        RecProbe::BothLow,
        |t| t.quarter_step(),
    )?;
    timing.quarter_backstep();
    program_rec_timing(mch, ch, group, timing)?;
    seek_rec(
        mch,
        ch,
        group,
        rank0,
        lanes,
        timing,
        RecProbe::AnyLow,
        |t| t.p += 1,
    )
}

fn reset_readwrite_pointers(mch: &Mchbar) {
    for ch in 0..2 {
        let drc1 = channel_offset(ch, 0x1234);
        mch.update32(drc1, 0, 1 << 6);
        mch.update32(drc1, 1 << 6, 0);
    }
    for pointer in [&mch.regs().write_pointer0, &mch.regs().write_pointer1] {
        pointer.set(pointer.get() & !(1 << 9));
        pointer.set(pointer.get() | (1 << 9));
        pointer.set(pointer.get() | (1 << 10));
    }
}

fn receive_enable_calibration(
    mch: &Mchbar,
    dimms: &Ddr3Channels,
    timing: &Ddr3Timing,
    bases: &[[Option<usize>; 2]; 2],
) -> Result<(), ServiceError> {
    for ch in 0..2 {
        if let Some(dimm) = dimms.dimms[ch] {
            let map = &REC_LANES[usize::from(dimm.raw_card == 5)];
            for (group, lanes) in map.iter().enumerate() {
                let lo = (lanes[0] as i32 - group as i32) as u32 & 3;
                let hi = (lanes[1] as i32 - group as i32 - 1) as u32 & 3;
                mch.update32(
                    rec_group_offset(ch, group),
                    (3 << 16) | (1 << 8) | 3,
                    lo | (hi << 16),
                );
            }
        }
    }
    let regs = mch.regs();
    for enable in [&regs.receive_enable0, &regs.receive_enable1] {
        enable.set(enable.get() | (1 << 31));
    }
    for pointer in [&regs.write_pointer0, &regs.write_pointer1] {
        pointer.set((pointer.get() & !(3 << 9)) | (1 << 9));
    }
    let result: Result<(), ServiceError> = (|| {
        for ch in 0..2 {
            let Some(dimm) = dimms.dimms[ch] else {
                continue;
            };
            let rank0 = bases[ch][0].ok_or(ServiceError::HardwareError)?;
            let card_f = dimm.raw_card == 5;
            let mut groups = [RecTiming::new(timing); 4];
            for (group, t) in groups.iter_mut().enumerate() {
                let lanes = REC_PROBE_LANES[usize::from(card_f)][group];
                program_rec_timing(mch, ch, group, t)?;
                seek_rec(mch, ch, group, rank0, lanes, t, RecProbe::AnyHigh, |t| {
                    t.quarter_step()
                })?;
                rec_edge(mch, ch, group, rank0, lanes, t)?;
                t.quarter_step();
                program_rec_timing(mch, ch, group, t)?;
                seek_rec(mch, ch, group, rank0, lanes, t, RecProbe::AnyHigh, |t| {
                    t.c -= 1
                })?;
                rec_edge(mch, ch, group, rank0, lanes, t)?;
                t.ph -= 2;
                t.normalize()?;
                if card_f {
                    t.t += 1;
                    program_rec_timing(mch, ch, group, t)?;
                }
            }
            let c_min = groups
                .iter()
                .map(|t| t.c)
                .min()
                .ok_or(ServiceError::HardwareError)?;
            for (group, t) in groups.iter_mut().enumerate() {
                t.pre = t.c - c_min;
                t.c = c_min;
                program_rec_timing(mch, ch, group, t)?;
            }
        }
        Ok(())
    })();
    for enable in [&regs.receive_enable0, &regs.receive_enable1] {
        enable.set(enable.get() & !(1 << 31));
    }
    reset_readwrite_pointers(mch);
    result?;
    // Coreboot lends CxDRT3[10:7] - 1 into CxDRT5[7:4]. Empty channels
    // have no trained C and must not silently wrap zero to 15.
    for ch in 0..2 {
        if dimms.dimms[ch].is_none() {
            continue;
        }
        let coarse = (mch.read32(channel_offset(ch, 0x121c)) >> 7) & 0xf;
        let lent = coarse.checked_sub(1).ok_or(ServiceError::HardwareError)?;
        mch.update32(channel_offset(ch, 0x1224), 0xf << 4, lent << 4);
    }
    Ok(())
}

/// Advance through DDR3 cold training, mapping and post-map register setup; fail closed.
pub(super) fn prepare_ddr3(
    base: u64,
    epbar: u64,
    revision: u8,
    fsb_mhz: u32,
    capid_high: u32,
    graphics: crate::gm45::Gm45GraphicsPolicy,
    dimms: &Ddr3Channels,
    timing: &Ddr3Timing,
) -> Result<(), ServiceError> {
    // This incomplete slice only implements non-SFF cold DDR3 initialization.
    if !matches!(revision, 5 | 6 | 7 | 9) || !matches!(fsb_mhz, 667 | 800 | 1067) {
        return Err(ServiceError::HardwareError);
    }
    if fsb_mhz == 1067 && timing.clock == MemClock::Mhz333 {
        return Err(ServiceError::HardwareError);
    }
    validate_register_timings(timing)?;
    let commands = ddr3_jedec_commands(timing)?;
    let bases = jedec_rank_bases(dimms)?;
    // Coreboot get_gmch_info() uses CPUID leaf 4 EAX[31:26] for core count.
    // Never guess whether the quad-core post-JEDEC register applies.
    if core::arch::x86_64::__cpuid(0).eax < 4 {
        return Err(ServiceError::HardwareError);
    }
    let cores = ((core::arch::x86_64::__cpuid_count(4, 0).eax >> 26) & 0x3f) + 1;
    if !matches!(cores, 1 | 2 | 4) {
        return Err(ServiceError::HardwareError);
    }
    // Check all derived values before beginning irreversible MMIO writes.
    crossing_values(fsb_mhz, timing.clock)?;
    let igd_enabled = graphics != crate::gm45::Gm45GraphicsPolicy::Discrete;
    let mch = Mchbar::new(base);
    let (kind, display_clock) = post_map::preflight(&mch, dimms, timing, graphics, capid_high)?;
    // Reject unsupported geometry before training, not after JEDEC/calibration.
    // ME presence is independent of the requested DEVEN policy.
    let me_active = EcamDevice::new(0, 3, 0).read8(0x08) != 0xff;
    let final_map = map::compute(dimms, igd_enabled, capid_high, me_active)?;
    let regs = mch.regs();
    regs.dram_ctrl.set(regs.dram_ctrl.get() | 3);
    set_memory_clock(&mch, timing.clock, fsb_mhz)?;
    if igd_enabled {
        set_igd_memory_frequencies(&mch, capid_high)?;
    }
    configure_dram_control(&mch, dimms, timing.clock);
    initialize_rcomp(&mch, revision)?;
    powerup_ddr3(&mch);
    program_timings(&mch, timing);
    program_banks(&mch, dimms);
    for (ch, dimm) in dimms.dimms.iter().enumerate() {
        if dimm.is_some() {
            mch.update32(channel_offset(ch, 0x120c), 0, 3);
        }
    }
    program_odt(&mch, timing);
    misc_settings(&mch, timing);
    clock_crossing_setup(&mch, fsb_mhz, timing.clock, dimms)?;
    vc1_program_timings(epbar, fsb_mhz)?;
    initialize_ddr3_io(&mch, dimms, timing.clock, revision);
    prejedec_memory_map(&mch, dimms);
    // SAFETY: ECAM was enabled by early host-bridge setup.
    let hb = unsafe { EcamDevice::new(0, 0, 0).regs::<super::Gm45HostBridgePciConfig>() };
    jedec_ddr3(&mch, hb, &bases, &commands);
    post_jedec_ddr3(&mch, hb, cores == 4);
    receive_enable_calibration(&mch, dimms, timing, &bases)?;
    if timing.clock == MemClock::Mhz533 {
        training::read_write_training(&mch, dimms, &bases)?;
    }
    map::program(&mch, hb, &final_map);
    post_map::program(
        &mch,
        hb,
        dimms,
        timing,
        final_map.interleaved,
        fsb_mhz,
        graphics,
        capid_high,
        kind,
        display_clock,
    )?;
    // Check the programmed decode against the computed geometry *after* the
    // post-map IGD policy (which may turn off graphics stolen memory). Only
    // then may the caller test the mapped DRAM and publish it as usable.
    let expected_ggc = if graphics == crate::gm45::Gm45GraphicsPolicy::Discrete {
        2
    } else {
        final_map.ggc
    };
    if hb.tom.get() & 0x1ff != final_map.tom
        || hb.tolud.get() & 0xfff0 != final_map.tolud
        || hb.touud.get() != final_map.touud
        || hb.remap_base.get() & 0x3ff != final_map.remap_base
        || hb.remap_limit.get() & 0x3ff != final_map.remap_limit
        || hb.esmramc.get() & 7 != 3
        || hb.ggc.get() & 0x0ff2 != expected_ggc & 0x0ff2
    {
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gm45_receive_timing_carries_and_rejects_invalid_fields() {
        let slow = Ddr3Timing {
            clock: MemClock::Mhz400,
            cas: 6,
            tck_256ns: 0,
            tras: 0,
            trp: 0,
            trcd: 0,
            twr: 0,
            trfc: 0,
            trd: 0,
            trrd: 0,
            tfaw: 0,
            twl: 0,
        };
        let mut t = RecTiming::new(&slow);
        assert_eq!((t.c, t.t_bound, t.p_bound), (7, 12, 1));
        t.quarter_step();
        t.normalize().unwrap();
        assert_eq!((t.c, t.ph, t.t, t.p), (7, 0, 6, 0));
        t.quarter_step();
        t.normalize().unwrap();
        assert_eq!((t.c, t.ph, t.t, t.p), (7, 2, 0, 0));
        t.ph -= 2;
        t.quarter_backstep();
        t.normalize().unwrap();
        assert_eq!((t.c, t.ph, t.t, t.p), (6, 2, 6, 0));
        let mut fast = RecTiming::new(&Ddr3Timing {
            clock: MemClock::Mhz533,
            ..slow
        });
        fast.quarter_step();
        fast.normalize().unwrap();
        assert_eq!((fast.t, fast.p), (4, 4));
        fast.p += 5;
        fast.normalize().unwrap();
        assert_eq!((fast.t, fast.p), (5, 1));
        fast.pre = 4;
        assert!(fast.normalize().is_err());
        fast.pre = 0;
        fast.c = -1;
        assert!(fast.normalize().is_err());
        fast.c = 16;
        assert!(fast.normalize().is_err());
    }

    #[test]
    fn gm45_receive_offsets_and_lane_maps_follow_coreboot() {
        assert_eq!(
            (rec_group_offset(0, 0), rec_group_offset(1, 3)),
            (0x14ac, 0x15a0)
        );
        assert_eq!(
            (rec_lane_offset(0, 0), rec_lane_offset(1, 7)),
            (0x14cc, 0x15b0)
        );
        assert_eq!(REC_LANES[1][1], [1, 3]);
        assert_eq!(REC_PROBE_LANES[1][1], [3, 3]);
        let mut t = RecTiming::new(&Ddr3Timing {
            clock: MemClock::Mhz533,
            cas: 6,
            tck_256ns: 0,
            tras: 0,
            trp: 0,
            trcd: 0,
            twr: 0,
            trfc: 0,
            trd: 0,
            trrd: 0,
            tfaw: 0,
            twl: 0,
        });
        t.t = 8;
        t.p = 7;
        t.ph = 2;
        t.pre = 3;
        assert_eq!(t.fields(), 0x87b0_0000);
        assert_eq!(core::mem::offset_of!(PreJedecRegs, receive_enable0), 0x12a4);
        assert_eq!(core::mem::offset_of!(PreJedecRegs, receive_enable1), 0x13a4);
    }

    #[test]
    fn gm45_ddr3_jedec_mrs_order_and_cycles_match_coreboot() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: false,
        };
        let dimms = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        let timing = select_ddr3_timing(&dimms, 800, 800).unwrap();
        assert_eq!(
            ddr3_jedec_commands(&timing).unwrap(),
            [
                JedecCommand {
                    selector: 0x240000,
                    address_bits: 0
                }, // MR2, WL5
                JedecCommand {
                    selector: 0x440000,
                    address_bits: 0
                }, // MR3
                JedecCommand {
                    selector: 0x040000,
                    address_bits: 0x210
                }, // MR1, ODT/ODS
                JedecCommand {
                    selector: 0x030000,
                    address_bits: 0x2940
                }, // MR0, DLL reset
                JedecCommand {
                    selector: 0x030000,
                    address_bits: 0x2140
                }, // MR0
            ]
        );
        let mut fast = timing;
        fast.twl = 6;
        fast.cas = 7;
        fast.twr = 12;
        let seq = ddr3_jedec_commands(&fast).unwrap();
        assert_eq!(seq[0].address_bits, 1 << 6);
        assert_eq!(seq[4].address_bits, (6 << 12) | (3 << 7) | (1 << 6));
        for twr in [4, 13] {
            fast.twr = twr;
            assert!(ddr3_jedec_commands(&fast).is_err());
        }
        assert_eq!(
            jedec_rank_bases(&dimms).unwrap(),
            [[Some(0), None], [None; 2]]
        );
        let mut both = dimms;
        both.dimms[0].as_mut().unwrap().ranks = 2;
        both.dimms[1] = both.dimms[0];
        assert_eq!(
            jedec_rank_bases(&both).unwrap(),
            [
                [Some(0), Some(128 << 20)],
                [Some(256 << 20), Some(384 << 20)],
            ]
        );
        both.dimms[1].as_mut().unwrap().ranks = 3;
        assert!(jedec_rank_bases(&both).is_err());
        assert_eq!(core::mem::offset_of!(PreJedecRegs, jedec_gate), 0x40);
        assert_eq!(core::mem::offset_of!(PreJedecRegs, write_pointer1), 0x15f0);
        assert_eq!(core::mem::offset_of!(PostJedecChannelRegs, control0), 0x28);
    }

    #[test]
    fn gm45_ddr3_clock_encodings_and_rcomp_lut_match_coreboot() {
        assert_eq!(
            RCOMP_DDR3_SEEDS,
            [
                (0x6ac, 0x55),
                (0x6ec, 0x66),
                (0x72c, 0x66),
                (0x76c, 0x66),
                (0x7ac, 0x66),
                (0x7ec, 0x66),
                (0x86c, 0x55),
                (0x8ac, 0x66),
            ]
        );
        assert_eq!(
            [
                clock_setting(MemClock::Mhz333),
                clock_setting(MemClock::Mhz400),
                clock_setting(MemClock::Mhz533)
            ],
            [4, 5, 6]
        );
        assert_eq!(rcomp_words(false, 7, 7), Some([0x0707_0707; 4]));
        assert_eq!(rcomp_words(true, 7, 7), Some([0x0505_0505; 4]));
        // B3 row 29/column 0 differs from conversion A1, in the second
        // packed dword of an index-29 (row 22) window.
        assert_eq!(rcomp_words(false, 29, 0).unwrap()[1], 0x0b0a_0909);
        assert_eq!(rcomp_words(true, 29, 0).unwrap()[1], 0x0a0a_0909);
        assert_eq!(rcomp_words(false, 55, 6), Some([0x0f0f_0f0f; 4]));
        assert!(rcomp_words(false, 6, 0).is_none());
        assert!(rcomp_words(true, 56, 0).is_none());
        assert!(rcomp_words(true, 7, 8).is_none());
        assert_eq!(channel_offset(1, 0x1230), 0x1330);
        // The electrical pull-down LUT comes first at each group address.
        assert_eq!(rcomp_table_offset(0, 0, 1), 0x680);
        assert_eq!(rcomp_table_offset(0, 0, 0), 0x698);
        assert_eq!(rcomp_table_offset(1, 0, 1), 0x840);
        assert_eq!(rcomp_table_offset(1, 0, 0), 0x858);
    }

    #[test]
    fn gm45_igd_fuses_and_vco_tables_match_coreboot() {
        // Variant 1, render fuse 1 => GM45. Variant 7 => PM45.
        assert_eq!(igd_variant((1 << 10) | (1 << 3)), Ok(0));
        assert_eq!(igd_variant(7 << 10), Ok(8));
        assert!(igd_variant((4 << 10) | (1 << 3)).is_err()); // GS45 power mode unknown
        assert!(igd_variant(5 << 10).is_err());
        assert_eq!(igd_frequency_codes(4, 0), Ok((13, 12, 1)));
        assert_eq!(igd_frequency_codes(2, 8), Ok((11, 10, 1)));
        assert!(igd_frequency_codes(3, 0).is_err());
    }

    #[test]
    fn gm45_crossing_pairs_reject_unsupported_clocks() {
        assert_eq!(
            crossing_values(1067, MemClock::Mhz533),
            Ok((0x0018_0006, 0x0081_0060, 0, 0x4010_0401))
        );
        assert_eq!(
            crossing_values(800, MemClock::Mhz333),
            Ok((0x0060_001c, 0x0003_0c00, 0x380, 0x0008_0201))
        );
        assert!(crossing_values(1067, MemClock::Mhz333).is_err());
        assert!(crossing_values(667, MemClock::Mhz400).is_err());
    }

    #[test]
    fn gm45_ddr3_register_fields_reject_unrepresentable_cycles() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: false,
        };
        let channels = probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).unwrap();
        let mut timing = select_ddr3_timing(&channels, 800, 800).unwrap();
        assert!(validate_register_timings(&timing).is_ok());
        timing.cas = 10; // DRT0 CAS field underflows on DDR3-800.
        assert!(validate_register_timings(&timing).is_err());
        timing.cas = 6;
        timing.tras = 32; // DRT1 tRAS field has five bits.
        assert!(validate_register_timings(&timing).is_err());
    }

    #[test]
    fn gm45_ddr3_io_tables_and_clock_mux_follow_coreboot() {
        assert_eq!(
            clock_mux_codes(MemClock::Mhz400, false, false),
            [2, 2, 2, 2, 0, 0, 0, 0].map(|v| v << 11)
        );
        assert_eq!(
            clock_mux_codes(MemClock::Mhz533, true, true),
            [6, 7, 6, 7, 4, 5, 4, 5].map(|v| v << 11)
        );
        assert_eq!(
            write_io_values(MemClock::Mhz333, 5, 0, false),
            [0xa3255008, 0x26888209, 0x26288208, 0x6188040f]
        );
        assert_eq!(
            write_io_values(MemClock::Mhz400, 9, 1, true),
            [0x7524240b, 0xb6256708, 0x132b7508, 0x5528040f]
        );
        assert_eq!(
            write_io_values(MemClock::Mhz533, 7, 1, false),
            [0xa4254208, 0x022b6108, 0x132b8208, 0x9228210f]
        );
        assert_eq!(io_channel_offset(1, 0x147c), 0x157c);
        assert_eq!(core::mem::offset_of!(PreJedecRegs, io0), 0x1400);
        assert_eq!(core::mem::offset_of!(PreJedecRegs, write_io2), 0x1590);
        assert_eq!(core::mem::offset_of!(PreJedecRegs, dcc), 0x200);
    }

    #[test]
    fn gm45_dummy_map_is_sequential_and_keeps_empty_rank_boundaries() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: true,
        };
        let mut one = probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).unwrap();
        one.dimms[0].as_mut().unwrap().ranks = 2;
        assert_eq!(
            dummy_rank_map(&one),
            (
                [[0x0008_0004, 0x0008_0008], [0x0008_0008, 0x0008_0008]],
                [0x22, 0],
                256
            )
        );
        let mut both = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        both.dimms[0].as_mut().unwrap().ranks = 2;
        assert_eq!(
            dummy_rank_map(&both),
            (
                [[0x0008_0004, 0x0008_0008], [0x000c_000c, 0x000c_000c]],
                [0x22, 2],
                384
            )
        );
        let second_only = Ddr3Channels {
            dimms: [None, both.dimms[1]],
        };
        assert_eq!(
            dummy_rank_map(&second_only),
            ([[0, 0], [0x0004_0004, 0x0004_0004]], [0, 2], 128)
        );
    }

    #[test]
    fn gm45_final_map_single_and_interleaved_rank_boundaries() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: true,
        };
        let one = probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).unwrap();
        let rank_mb = one.dimms[0].unwrap().rank_capacity_mb;
        let single = map::compute(&one, false, 1 << 15, false).unwrap();
        assert!(!single.interleaved);
        assert_eq!(single.bounds[0][0], ((rank_mb >> 5) & 0x1fc) * 0x1_0001);
        assert_eq!(single.bounds[1][0], single.bounds[0][0]);
        assert_eq!(single.pages, [3, 0]); // SPD 1 KiB/chip becomes 8 KiB/rank.
        assert_eq!(single.tom, (rank_mb >> 7) as u16);
        assert_eq!(single.touud, rank_mb as u16);
        assert_eq!(single.remap_base, 0x3ff);
        assert_eq!(single.ggc, 2);
        assert_eq!(map::compute(&one, true, 1 << 1, false).unwrap().ggc, 2);
        let mut both = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        both.dimms[0].as_mut().unwrap().ranks = 2;
        both.dimms[1].as_mut().unwrap().ranks = 2;
        let dual = map::compute(&both, true, 0, false).unwrap();
        assert!(dual.interleaved);
        assert_eq!(dual.bounds[0], dual.bounds[1]);
        assert_eq!(
            dual.bounds[0][0],
            ((rank_mb * 2 >> 5) << 16) | (rank_mb >> 5)
        );
        assert_eq!(dual.ggc, 0x0b50);
        assert_eq!(dual.tom, (rank_mb * 4 >> 7) as u16);
        assert_eq!(dual.tolud, 2048 << 4);
        assert_eq!(dual.touud, (rank_mb * 4 + 2048) as u16);
        assert_eq!(dual.remap_base, (rank_mb * 4 >> 6) as u16);
        assert_eq!(dual.remap_limit, ((rank_mb * 4 + 2048 - 64) >> 6) as u16);
    }

    #[test]
    fn gm45_final_map_me_reclaim_and_e820_boundaries() {
        use fstart_core::services::memory_detect::{E820Entry, E820Kind, build_pc_compatible_e820};
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: true,
        };
        let mut both = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        both.dimms[0].as_mut().unwrap().ranks = 2;
        both.dimms[1].as_mut().unwrap().ranks = 2;
        let map = map::compute(&both, true, 1 << 16, true).unwrap();
        // No VT-d shadow GTT: 32 MiB UMA + 2 MiB GTT + 2 MiB TSEG.
        assert_eq!(map.ggc, 0x0350);
        assert_eq!(map.tolud, 2048 << 4);
        assert_eq!(map.remap_base, 4096 >> 6);
        assert_eq!(map.touud, 6080); // 4096 + (4096 - 2048) - 64 MiB ME
        let usable = (u32::from(map.tolud) >> 4) - 36;
        let mut entries = [E820Entry::zeroed(); 8];
        let n = build_pc_compatible_e820(
            &mut entries,
            usable << 20,
            u64::from(map.touud) << 20,
            u32::from(map.tolud) << 16,
        )
        .unwrap();
        assert!(entries[..n].iter().any(|e| e.kind == E820Kind::Ram as u32
            && e.addr == 0x1_0000_0000
            && e.size == u64::from(map.touud - 4096) << 20));
        assert!(
            entries[..n]
                .iter()
                .any(|e| e.kind == E820Kind::Reserved as u32
                    && e.addr == u64::from(usable) << 20
                    && e.size == 36 << 20)
        );
    }

    #[test]
    fn gm45_final_map_rejects_asymmetry_and_invalid_geometry() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: true,
        };
        let mut both = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        both.dimms[1].as_mut().unwrap().ranks = 2;
        assert!(map::compute(&both, false, 0, false).is_err());
        both.dimms[1].as_mut().unwrap().ranks = 1;
        both.dimms[0].as_mut().unwrap().rank_capacity_mb = 33;
        assert!(map::compute(&both, false, 0, false).is_err());
        both.dimms[0].as_mut().unwrap().rank_capacity_mb = 1024;
        both.dimms[0].as_mut().unwrap().page_size = 0;
        assert!(map::compute(&both, false, 0, false).is_err());
    }

    #[test]
    fn gm45_training_addresses_follow_dummy_rank_boundaries() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: true,
        };
        let mut dimms = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        dimms.dimms[0].as_mut().unwrap().ranks = 2;
        let bases = jedec_rank_bases(&dimms).unwrap();
        let bunches = training::rank_addresses(&dimms, &bases).unwrap();
        assert!(bunches[0].iter().copied().eq([0, 128 << 20]));
        assert!(bunches[1].iter().copied().eq([256 << 20]));
        let mut invalid = bases;
        invalid[1][0] = Some(128 << 20);
        assert!(training::rank_addresses(&dimms, &invalid).is_err());
        dimms.dimms[0].as_mut().unwrap().ranks = 3;
        assert!(training::rank_addresses(&dimms, &bases).is_err());
    }

    struct FakeBus {
        spd: [u8; 256],
        extra_slot: bool,
    }
    impl SmBus for FakeBus {
        fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
            if addr != 0x50 && !(addr == 0x51 && self.extra_slot) {
                return Err(ServiceError::HardwareError);
            }
            Ok(self.spd[cmd as usize])
        }
        fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
            Ok(())
        }
    }
    fn spd() -> [u8; 256] {
        let mut spd = [0; 256];
        spd[2] = DDR3;
        spd[3] = 3;
        spd[4] = 2;
        spd[5] = (2 << 3) | 1;
        spd[7] = 1;
        spd[8] = 3;
        spd[10] = 1;
        spd[11] = 8;
        spd[12] = 12; // DDR3-667
        spd[14] = 0b0011_1100; // CL6..9
        spd[16] = 100;
        spd[17] = 120;
        spd[18] = 110;
        spd[20] = 110;
        spd[21] = 1;
        spd[22] = 0x18;
        spd[24] = 0xa0;
        spd[25] = 4;
        spd
    }
    #[test]
    fn gm45_spd_discovery_rejects_extra_slot_and_mixed_type() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: false,
        };
        assert!(probe_ddr3(&mut bus, &[0x50, 0x51, 0, 0]).is_ok());
        bus.extra_slot = true;
        assert!(probe_ddr3(&mut bus, &[0x50, 0x51, 0, 0]).is_err());
        bus.spd[2] = crate::generic::spd::ddr2::DDR2;
        assert!(probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).is_err());
    }
    #[test]
    fn gm45_clock_cas_and_derived_cycles_follow_coreboot_tables() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: false,
        };
        let channels = probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).unwrap();
        let t = select_ddr3_timing(&channels, 800, 800).unwrap();
        assert_eq!(
            (t.clock, t.cas, t.trfc, t.trd, t.trrd, t.tfaw, t.twl),
            (MemClock::Mhz400, 6, 51, 8, 3, 15, 5)
        );
        assert_eq!((t.tras, t.trp, t.trcd, t.twr), (14, 6, 6, 6));
        assert!(select_ddr3_timing(&channels, 533, 800).is_err());
        bus.spd[14] = 0;
        let channels = probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).unwrap();
        assert!(select_ddr3_timing(&channels, 800, 800).is_err());
    }

    #[test]
    fn gm45_dual_channel_uses_slowest_spd_and_rejects_unsupported_straps() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: true,
        };
        let mut channels = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        channels.dimms[1].as_mut().unwrap().trp_256ns += 640;
        channels.dimms[1].as_mut().unwrap().cas_latencies &= !(1 << 6);
        let t = select_ddr3_timing(&channels, 800, 800).unwrap();
        assert_eq!((t.clock, t.cas, t.trp), (MemClock::Mhz400, 7, 7));
        assert!(select_ddr3_timing(&channels, 800, 0).is_err());
        assert!(select_ddr3_timing(&channels, 533, 800).is_err());
        channels.dimms[0].as_mut().unwrap().page_size = 4096;
        assert!(select_ddr3_timing(&channels, 800, 800).is_err());
    }
}
