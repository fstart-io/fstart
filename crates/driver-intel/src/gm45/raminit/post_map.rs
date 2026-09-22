//! Cold DDR3 post-map programming from coreboot gm45/raminit.c, thermal.c and igd.c.
//! This does not attest DRAM integrity or initialize a display pipeline/VBT.

use super::*;
use crate::generic::spd::ChipWidth;
use crate::gm45::Gm45GraphicsPolicy;

register_structs! {
    PostMapRegs {
        (0x0000 => _pad0: [u8; 0xbd3]),
        (0x0bd3 => hsync: MmioReadWrite<u8>),
        (0x0bd4 => vsync: MmioReadWrite<u8>),
        (0x0bd5 => _pad1: [u8; 0x3f]),
        (0x0c14 => peg_gate: MmioReadWrite<u8>),
        (0x0c15 => _pad2: [u8; 0x3ec]),
        (0x1001 => thermal0_lo: MmioReadWrite<u8>),
        (0x1002 => thermal0_hi: MmioReadWrite<u8>),
        (0x1003 => _pad3: [u8; 4]),
        (0x1007 => thermal1: MmioReadWrite<u8>),
        (0x1008 => _pad4: [u8; 8]),
        (0x1010 => thermal2: MmioReadWrite<u32>),
        (0x1014 => thermal3: MmioReadWrite<u32>),
        (0x1018 => _pad5: [u8; 4]),
        (0x101c => thermal4: MmioReadWrite<u8>),
        (0x101d => _pad6: [u8; 0x24]),
        (0x1041 => thermal5_lo: MmioReadWrite<u8>),
        (0x1042 => thermal5_hi: MmioReadWrite<u8>),
        (0x1043 => _pad7: [u8; 4]),
        (0x1047 => thermal6: MmioReadWrite<u8>),
        (0x1048 => _pad8: [u8; 8]),
        (0x1050 => thermal7: MmioReadWrite<u32>),
        (0x1054 => thermal8: MmioReadWrite<u32>),
        (0x1058 => _pad9: [u8; 4]),
        (0x105c => thermal9: MmioReadWrite<u8>),
        (0x105d => _pad10: [u8; 0x133]),
        (0x1190 => gm49_control: MmioReadWrite<u16>),
        (0x1192 => _pad11: [u8; 0x1e]),
        (0x11b0 => thermal_control: MmioReadWrite<u32>),
        (0x11b4 => _pad12: [u8; 0x1c]),
        (0x11d0 => fsb_thermal: MmioReadWrite<u32>),
        (0x11d4 => fsb_thermal_control: MmioReadWrite<u32>),
        (0x11d8 => @END),
    }
}

register_structs! {
    ThermalGlobalRegs {
        (0x00 => _pad: [u8; 0x70]),
        (0x70 => sensor_enable: MmioReadWrite<u8>),
        (0x71 => _pad2: [u8; 0xf]),
        (0x80 => sensor_config: MmioReadWrite<u8>),
        (0x81 => @END),
    }
}

register_structs! {
    IgdPciRegs {
        (0x000 => vendor: MmioReadWrite<u16>),
        (0x002 => _pad0: [u8; 0xca]),
        (0x0cc => display_clock: MmioReadWrite<u16>),
        (0x0ce => _pad1: [u8; 0x22]),
        (0x0f0 => clock_fuse: MmioReadWrite<u16>),
        (0x0f2 => @END),
    }
}

register_structs! {
    PegPciRegs {
        (0x000 => _pad0: [u8; 0x200]),
        (0x200 => control: MmioReadWrite<u32>),
        (0x204 => _pad1: [u8; 0x10]),
        (0x214 => status: MmioReadWrite<u32>),
        (0x218 => _pad2: [u8; 0xc]),
        (0x224 => link_control: MmioReadWrite<u16>),
        (0x226 => _pad3: [u8; 0x7e2]),
        (0xa08 => igd_control: MmioReadWrite<u16>),
        (0xa0a => _pad4: [u8; 0x7a]),
        (0xa84 => igd_link: MmioReadWrite<u16>),
        (0xa86 => _pad5: [u8; 0x7a]),
        (0xb00 => igd_bridge: MmioReadWrite<u16>),
        (0xb02 => _pad6: [u8; 0x12]),
        (0xb14 => igd_misc: MmioReadWrite<u32>),
        (0xb18 => @END),
    }
}

register_structs! {
    LpcPciRegs {
        (0x00 => _pad: [u8; 0xa2]),
        (0xa2 => gen_pmcon_2: MmioReadWrite<u8>),
        (0xa3 => @END),
    }
}

fn variant(capid_high: u32, graphics: Gm45GraphicsPolicy) -> Result<usize, ServiceError> {
    let kind = igd_variant(capid_high)?;
    let fuse_disabled = capid_high & (1 << 1) != 0;
    if graphics != Gm45GraphicsPolicy::Discrete && (fuse_disabled || kind == 8) {
        return Err(ServiceError::HardwareError);
    }
    Ok(kind)
}

// thermal.c: four words per populated channel, indexed by width, density and speed.
fn thermal_words(clock: MemClock, width: ChipWidth, density: u8) -> Option<[u32; 4]> {
    let words = match (clock, width, density) {
        (MemClock::Mhz533, ChipWidth::X16, _) => [0x0d0b0403, 0x060d, 0x2d0b221a, 0xc779956e],
        (MemClock::Mhz533, ChipWidth::X8, d) => [
            0x06040101,
            0x0506,
            if d == 3 { 0xa1071416 } else { 0x1a071416 },
            0x7246643f,
        ],
        (MemClock::Mhz400, ChipWidth::X16, _) => [0x06030100, 0x0506, 0x3e081714, 0xbb79a171],
        (MemClock::Mhz400, ChipWidth::X8, 0 | 1) => [0x05050101, 0x0503, 0x1e08170d, 0x502f3827],
        (MemClock::Mhz400, ChipWidth::X8, 2) => [0x05060101, 0x0503, 0x3306130e, 0x5763485d],
        (MemClock::Mhz400, ChipWidth::X8, 3) => [0x05060101, 0x0503, 0x57051010, 0x5fd15dde],
        (MemClock::Mhz333, ChipWidth::X16, _) => [0x02000000, 0x0402, 0x46061111, 0xb579a772],
        (MemClock::Mhz333, ChipWidth::X8, 0 | 1) => [0x04070101, 0x0501, 0x20081808, 0x3f23221b],
        (MemClock::Mhz333, ChipWidth::X8, 2) => [0x04070101, 0x0501, 0x3f05120a, 0x49713a6c],
        (MemClock::Mhz333, ChipWidth::X8, 3) => [0x04070101, 0x0501, 0x32040e0d, 0x55ff59ff],
        _ => return None,
    };
    (density <= 3).then_some(words)
}

fn gate_weight(clock: MemClock, interleaved: bool) -> u32 {
    match (clock, interleaved) {
        (MemClock::Mhz533, true) => 0xc8f81717,
        (MemClock::Mhz400, true) => 0x96ba1717,
        (MemClock::Mhz333, true) => 0x7d9b1717,
        (MemClock::Mhz533, false) => 0x53661717,
        (MemClock::Mhz400, false) => 0x886e1717,
        (MemClock::Mhz333, false) => 0x38621717,
    }
}

fn thermal(
    mch: &Mchbar,
    dimms: &Ddr3Channels,
    timing: &Ddr3Timing,
    interleaved: bool,
    fsb_mhz: u32,
    kind: usize,
) {
    for (ch, dimm) in dimms.dimms.iter().enumerate() {
        let Some(dimm) = dimm else { continue };
        let words = thermal_words(timing.clock, dimm.width, dimm.density_code)
            .expect("validated before programming");
        for (index, value) in words.iter().enumerate() {
            mch.write32(channel_offset(ch, 0x1278 + index * 4), *value);
        }
        mch.write32(
            channel_offset(ch, 0x1270),
            gate_weight(timing.clock, interleaved),
        );
    }
    for ch in 0..2 {
        mch.write32(channel_offset(ch, 0x1288), 0x00004020);
    }
    for ch in 0..2 {
        mch.write32(channel_offset(ch, 0x1274), 0x00304848);
    }
    for (offset, mask) in [(0x1290, 0xa4810007), (0x12b4, 0xa2810007)] {
        for ch in 0..2 {
            mch.update32(channel_offset(ch, offset), !0xfff8, mask);
        }
    }
    // SAFETY: MCHBAR decode is enabled, and the fixed registers lie within 16 KiB.
    let global = unsafe { &*((mch.base + 0x1000) as *const ThermalGlobalRegs) };
    global.sensor_enable.set(1);
    global.sensor_config.set(6);
    let regs = unsafe { &*(mch.base as *const PostMapRegs) };
    let pm45 = kind == 8;
    regs.thermal0_lo.set(0);
    regs.thermal0_hi.set(if pm45 { 0 } else { 0x92 });
    regs.thermal1.set(0);
    regs.thermal2.set(if pm45 { 0 } else { 0x2309 });
    regs.thermal3.set(0);
    regs.thermal4.set(0x98);
    regs.thermal5_lo.set(0);
    regs.thermal5_hi.set(if pm45 { 0x92 } else { 0 });
    regs.thermal6.set(0);
    regs.thermal7.set(if pm45 { 0x2309 } else { 0 });
    regs.thermal8.set(0);
    regs.thermal9.set(0x98);
    regs.thermal2.set(regs.thermal2.get() | (1 << 31));
    regs.thermal7.set(regs.thermal7.get() | (1 << 31));
    for ch in 0..2 {
        mch.update32(channel_offset(ch, 0x1274), 0, 1 << 31);
    }
    if kind == 2 {
        // GM49
        regs.thermal_control.set(0x2000383a);
        regs.gm49_control.set(regs.gm49_control.get() & !(1 << 15));
    } else if kind != 8 {
        // PM45 has no 0x11b0 programming
        regs.thermal_control.set(0xa000383a);
    }
    regs.fsb_thermal.set(match fsb_mhz {
        667 => 0x0fd88000,
        800 => 0x1303c000,
        1067 => 0x194a0000,
        _ => unreachable!("validated FSB"),
    });
    regs.fsb_thermal_control
        .set((regs.fsb_thermal_control.get() & !0x1f) | 4);
}

fn display_clock(vco: u8, f0: u16) -> Result<u16, ServiceError> {
    let index = match vco & 7 {
        4 => 0,
        0 => 1,
        1 => 2,
        2 => 3,
        _ => return Err(ServiceError::HardwareError),
    };
    Ok([[222, 228, 222, 222], [333, 320, 333, 333]][usize::from(f0 & (1 << 12) != 0)][index])
}

fn init_igd(
    mch: &Mchbar,
    hb: &super::super::Gm45HostBridgePciConfig,
    graphics: Gm45GraphicsPolicy,
    kind: usize,
    clock: Option<u16>,
    capid_high: u32,
) -> Result<(), ServiceError> {
    let regs = unsafe { &*(mch.base as *const PostMapRegs) };
    if graphics == Gm45GraphicsPolicy::Discrete {
        hb.ggc.set((hb.ggc.get() & 0xff0f) | 2);
        mch.regs().igd_gate.set(mch.regs().igd_gate.get() | 1);
        if capid_high & (1 << 1) == 0 {
            regs.gm49_control.set(regs.gm49_control.get() | (1 << 14));
            mch.regs()
                .igd_index
                .set((mch.regs().igd_index.get() & !(7 << 13)) | (4 << 13) | (1 << 12));
        }
        hb.deven.set(hb.deven.get() & !((1 << 3) | (1 << 4)));
        return Ok(());
    }
    // SAFETY: ECAM was installed before DRAM init; offsets fit a function's 4 KiB window.
    let igd = unsafe { EcamDevice::new(0, 2, 0).regs::<IgdPciRegs>() };
    regs.hsync.set(0x5a);
    regs.vsync.set(0x5a);
    igd.display_clock
        .set((igd.display_clock.get() & 0xfc00) | clock.ok_or(ServiceError::HardwareError)?);
    // map::program already installed the computed GGC before this step.
    // GL40/GS40/GL43 do not have the PEG link registers below.
    if matches!(kind, 4..=6) {
        return Ok(());
    }
    let peg = unsafe { EcamDevice::new(0, 1, 0).regs::<PegPciRegs>() };
    let deven = hb.deven.get();
    if deven & 2 == 0 {
        hb.deven.set(deven | 2);
    }
    peg.igd_control.set(peg.igd_control.get() & !(1 << 15));
    peg.igd_link.set(peg.igd_link.get() | (1 << 8));
    peg.igd_bridge
        .set(peg.igd_bridge.get() | (3 << 8) | (7 << 3));
    peg.igd_misc.set(peg.igd_misc.get() & !(1 << 17));
    // Coreboot also restores a PEG function that was initially hidden when
    // hybrid graphics is selected; the temporary DEVEN enable is not policy.
    if deven & 2 == 0 || graphics == Gm45GraphicsPolicy::Integrated {
        regs.peg_gate.set(regs.peg_gate.get() | (1 << 5) | 1);
        peg.control.set(peg.control.get() | (1 << 18));
        peg.link_control.set(peg.link_control.get() | (1 << 8));
        peg.control.set(peg.control.get() & !(1 << 18));
        // Coreboot polls 0x214[19:16] until PEG is idle. Bound the wait to fail closed.
        let mut idle = false;
        for _ in 0..100_000 {
            if peg.status.get() & (0xf << 16) == 0 {
                idle = true;
                break;
            }
        }
        if !idle {
            return Err(ServiceError::HardwareError);
        }
        hb.deven.set(deven & !2);
        regs.peg_gate.set(regs.peg_gate.get() & !((1 << 5) | 1));
    }
    Ok(())
}

pub(super) fn preflight(
    mch: &Mchbar,
    dimms: &Ddr3Channels,
    timing: &Ddr3Timing,
    graphics: Gm45GraphicsPolicy,
    capid_high: u32,
) -> Result<(usize, Option<u16>), ServiceError> {
    let kind = variant(capid_high, graphics)?;
    for dimm in dimms.dimms.iter().flatten() {
        thermal_words(timing.clock, dimm.width, dimm.density_code)
            .ok_or(ServiceError::HardwareError)?;
    }
    let clock = if graphics == Gm45GraphicsPolicy::Discrete {
        None
    } else {
        // SAFETY: ECAM was installed before DRAM init.
        let igd = unsafe { EcamDevice::new(0, 2, 0).regs::<IgdPciRegs>() };
        if igd.vendor.get() != 0x8086 {
            return Err(ServiceError::HardwareError);
        }
        Some(display_clock(
            mch.regs().hpll_vco.get(),
            igd.clock_fuse.get(),
        )?)
    };
    Ok((kind, clock))
}

pub(super) fn program(
    mch: &Mchbar,
    hb: &super::super::Gm45HostBridgePciConfig,
    dimms: &Ddr3Channels,
    timing: &Ddr3Timing,
    interleaved: bool,
    fsb_mhz: u32,
    graphics: Gm45GraphicsPolicy,
    capid_high: u32,
    kind: usize,
    clock: Option<u16>,
) -> Result<(), ServiceError> {
    for (ch, dimm) in dimms.dimms.iter().enumerate() {
        if let Some(dimm) = dimm {
            mch.update32(
                channel_offset(ch, 0x1234),
                0xff << 24,
                (if dimm.ranks == 1 { 0xb1 } else { 0x91 }) << 24,
            );
        }
    }
    // coreboot raminit.c: clear ICH9 LPC GEN_PMCON_2 bit 7 only after final map/optimizations.
    // SAFETY: ECAM was installed before DRAM init.
    let lpc = unsafe { EcamDevice::new(0, 0x1f, 0).regs::<LpcPciRegs>() };
    lpc.gen_pmcon_2.set(lpc.gen_pmcon_2.get() & !(1 << 7));
    thermal(mch, dimms, timing, interleaved, fsb_mhz, kind);
    init_igd(mch, hb, graphics, kind, clock, capid_high)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gm45_thermal_tables_and_modes() {
        assert_eq!(
            thermal_words(MemClock::Mhz533, ChipWidth::X8, 3),
            Some([0x06040101, 0x0506, 0xa1071416, 0x7246643f])
        );
        assert_eq!(
            thermal_words(MemClock::Mhz400, ChipWidth::X8, 0),
            Some([0x05050101, 0x0503, 0x1e08170d, 0x502f3827])
        );
        assert_eq!(
            thermal_words(MemClock::Mhz333, ChipWidth::X16, 2),
            Some([0x02000000, 0x0402, 0x46061111, 0xb579a772])
        );
        assert_eq!(
            thermal_words(MemClock::Mhz333, ChipWidth::X8, 3),
            Some([0x04070101, 0x0501, 0x32040e0d, 0x55ff59ff])
        );
        assert_eq!(thermal_words(MemClock::Mhz400, ChipWidth::X8, 4), None);
        assert_eq!(gate_weight(MemClock::Mhz533, true), 0xc8f81717);
        assert_eq!(gate_weight(MemClock::Mhz533, false), 0x53661717);
        assert_eq!(gate_weight(MemClock::Mhz400, true), 0x96ba1717);
        assert_eq!(gate_weight(MemClock::Mhz333, false), 0x38621717);
        assert_eq!(core::mem::offset_of!(PostMapRegs, thermal0_lo), 0x1001);
        assert_eq!(core::mem::offset_of!(PostMapRegs, thermal_control), 0x11b0);
        assert_eq!(core::mem::offset_of!(PegPciRegs, igd_misc), 0xb14);
        assert_eq!(core::mem::offset_of!(LpcPciRegs, gen_pmcon_2), 0xa2);
    }
    #[test]
    fn gm45_igd_fuse_policy_and_display_clocks() {
        assert_eq!(
            variant(7 << 10 | 1 << 1, Gm45GraphicsPolicy::Discrete),
            Ok(8)
        );
        assert!(variant(7 << 10, Gm45GraphicsPolicy::Integrated).is_err());
        assert!(variant(1 << 10 | 1 << 3 | 1 << 1, Gm45GraphicsPolicy::Hybrid).is_err());
        assert!(variant(4 << 10 | 1 << 3, Gm45GraphicsPolicy::Discrete).is_err());
        assert_eq!(display_clock(4, 0), Ok(222));
        assert_eq!(display_clock(0, 1 << 12), Ok(320));
        assert_eq!(display_clock(1, 0), Ok(222));
        assert_eq!(display_clock(2, 1 << 12), Ok(333));
        assert!(display_clock(7, 0).is_err());
    }
}
