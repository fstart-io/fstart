//! SATA (00:13.0), AHCI or IDE (coreboot `sata.c`).

use super::{Bar, Function};
use crate::MmioBar;
use fstart_pci::ecam::EcamDevice;

/// SATA policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SataConfig {
    /// Enabled ports, one bit per port (two ports).
    pub port_map: u8,
    pub ahci: bool,
    /// IDE mode: legacy combined decoding instead of native.
    pub ide_legacy_combined: bool,
}

/// Indexed SATA initialization registers.
const SIRI: u16 = 0xa0;
const SIRD: u16 = 0xa4;
const MAP: u16 = 0x90;
const PCS: u16 = 0x92;
const TM: u16 = 0x94;
const TM2: u16 = 0x98;
const SATAGC: u16 = 0x9c;

fn device() -> EcamDevice {
    Function::Sata.device()
}

fn sir_write(sata: &EcamDevice, index: u32, value: u32) {
    sata.write32(SIRI, index);
    sata.write32(SIRD, value);
}

/// Before enumeration: port mapping, test modes and the requested ports
/// (coreboot `sata_enable`).
pub fn enable(config: &SataConfig) {
    let sata = device();
    if !sata.is_present() {
        return;
    }
    super::set_enabled(Function::Sata, true);
    let map = u16::from(config.port_map);
    let mut reg = sata.read16(MAP) & !0x03e0;
    reg |= (map ^ 0x3) << 8;
    if config.ahci {
        reg |= 0x60;
    }
    sata.write16(MAP, reg);
    sata.modify32(TM, !0x1ff, 0x183);
    sata.or16(PCS, 1 << 15);
    // TM2: undocumented, per the reference code.
    sata.modify32(
        TM2,
        !(0x1f80 | 1 << 6 | 1 << 5),
        1 << 29 | 1 << 25 | 1 << 23 | 1 << 22 | 1 << 20 | 1 << 19 | 1 << 18 | 1 << 9 | 1 << 5,
    );
    sata.or8(0x04, 1 << 2);
    // Test modes, OOB detection margin and gasket control.
    for (index, value) in [
        (0x70, 0x0028_8301),
        (0x54, 0x0000_0300),
        (0x58, 0x5000_0000),
        (0x6c, 0x130c_0603),
        (0xf4, 0),
    ] {
        sir_write(&sata, index, value);
    }
    sata.and8_or8(PCS, !0x03, config.port_map);
}

/// After allocation: decode, AHCI capabilities and ports, clocks and the
/// SATAGC lock (coreboot `sata_init`).
pub fn init(config: &SataConfig) {
    let sata = device();
    if !sata.is_present() {
        return;
    }
    fstart_log::debug!("baytrail: SATA init");
    if !config.ahci {
        // Legacy (combined) or native IDE decoding.
        let progif = sata.read8(0x09);
        sata.write8(
            0x09,
            if config.ide_legacy_combined {
                progif & !0x5
            } else {
                progif | 0x5
            },
        );
        sata.write8(0x34, 0x70);
        sata.and16(0x70, !0xff00);
    }
    // Primary and secondary timing decode enable.
    sata.or16(0x40, 1 << 15);
    sata.or16(0x42, 1 << 15);
    let map = u16::from(config.port_map);
    sata.or16(MAP, (map ^ 0x3) << 8);
    sata.write16(PCS, (sata.read16(PCS) & !0x003f) | map);

    if config.ahci {
        if let Some(abar) = sata.memory_bar(0x24).map(|base| Bar(base as usize)) {
            sata.or16(0x04, 0x2);
            // PSC, SSC, SALP, SSS, SAM; no SXS, EMS, PMS; Gen 3.
            let cap = (abar.read32(0x00) | 0x0c04_6000) & !0x00f2_0060;
            abar.write32(0x00, cap | 3 << 20);
            abar.write32(
                0x0c,
                (abar.read32(0x0c) & !0x3f) | u32::from(config.port_map),
            );
            // Two extra reads latch the port enables.
            let _ = abar.read32(0x0c);
            let _ = abar.read32(0x0c);
            // CAP2: DevSleep.
            abar.write32(0x24, 1 << 5 | 1 << 4 | 1 << 3);
            for port in [0x118, 0x198] {
                let cmd = abar.read32(port)
                    & !(1 << 27 | 1 << 26 | 1 << 22 | 1 << 21 | 1 << 19 | 1 << 18 | 1 << 1);
                abar.write32(port, cmd | 2);
            }
            // No reset features; interrupts on.
            abar.write32(0xc8, 0);
            abar.setbits8(0x04, 0x02);
        } else {
            fstart_log::warn!("baytrail: SATA ABAR unassigned");
        }
    }
    // 1.4 us after the port and enable bits.
    fstart_arch::x86::udelay(2);
    sata.and32(TM, !(u32::from(config.port_map) << 24));
    sata.write32(SATAGC, 1 << 31 | 0x7 << 12);
}
