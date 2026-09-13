//! Legacy GMCH port-detect and hotplug helpers.
//!
//! This mirrors the small G45-family libgfxinit `Port_Detect` layer used before
//! probing EDID: analog is always considered valid, GM965/GM45 LVDS is valid by
//! platform policy, and digital GMCH ports are accepted only when their port
//! control register reports the `PORT_DETECTED` strap/status bit.  The live path
//! also enables hotplug status generation for the detected ports.

use tock_registers::interfaces::{Readable, Writeable};

use crate::error::GmaError;
use crate::mmio::Mmio;
use crate::regs::{GmchHotplugRegs, PORT_HOTPLUG_EN as HP_EN, PORT_HOTPLUG_STAT as HP_STAT};
use crate::types::{Cpu, Port};

const PORT_DETECTED: u32 = HP_STAT::PORT_DETECTED::SET.value;

const PORTB_HOTPLUG_INT_EN: u32 = HP_EN::PORTB_HOTPLUG_INT_EN::SET.value;
const PORTC_HOTPLUG_INT_EN: u32 = HP_EN::PORTC_HOTPLUG_INT_EN::SET.value;
const PORTD_HOTPLUG_INT_EN: u32 = HP_EN::PORTD_HOTPLUG_INT_EN::SET.value;
const SDVOB_HOTPLUG_INT_EN: u32 = HP_EN::SDVOB_HOTPLUG_INT_EN::SET.value;
const SDVOC_HOTPLUG_INT_EN: u32 = HP_EN::SDVOC_HOTPLUG_INT_EN::SET.value;
const CRT_HOTPLUG_INT_EN: u32 = HP_EN::CRT_HOTPLUG_INT_EN::SET.value;
const CRT_HOTPLUG_ACTIVATION_PERIOD_64: u32 = HP_EN::CRT_HOTPLUG_ACTIVATION_PERIOD_64::SET.value;

/// GMCH PORT_HOTPLUG_EN register offset.
pub(crate) const PORT_HOTPLUG_EN: usize = 0x61110;

/// Result of one legacy GMCH port-detect pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegacyPortDetectState {
    /// Ports accepted by platform policy and live detect bits.
    pub valid_ports: [Option<Port>; 8],
    /// Number of entries in `valid_ports`.
    pub valid_len: usize,
    /// Value programmed to PORT_HOTPLUG_EN.
    pub hotplug_enable: u32,
}

impl LegacyPortDetectState {
    /// Return true if the pass accepted `port` as a possible connector.
    pub const fn is_valid(self, port: Port) -> bool {
        let mut index = 0usize;
        while index < self.valid_len {
            if let Some(candidate) = self.valid_ports[index]
                && candidate as u8 == port as u8
            {
                return true;
            }
            index += 1;
        }
        false
    }

    const fn push(mut self, port: Port) -> Self {
        if self.valid_len < self.valid_ports.len() {
            self.valid_ports[self.valid_len] = Some(port);
            self.valid_len += 1;
        }
        self
    }
}

/// Compute the libgfxinit-style valid-port and hotplug-enable state from GMCH
/// port-control register snapshots.
pub(crate) const fn legacy_gmch_detect_state(
    cpu: Cpu,
    hdmib: u32,
    hdmic: u32,
    dpb: u32,
    dpc: u32,
    dpd: u32,
) -> LegacyPortDetectState {
    let mut state = LegacyPortDetectState {
        valid_ports: [None; 8],
        valid_len: 0,
        hotplug_enable: CRT_HOTPLUG_INT_EN | CRT_HOTPLUG_ACTIVATION_PERIOD_64,
    }
    .push(Port::Vga);

    if matches!(cpu, Cpu::Gm965 | Cpu::Gm45) {
        state = state.push(Port::Lvds);
    }
    if (hdmib & PORT_DETECTED) != 0 {
        state = state.push(Port::HdmiA);
        state.hotplug_enable |= SDVOB_HOTPLUG_INT_EN;
    }
    if (hdmic & PORT_DETECTED) != 0 {
        state = state.push(Port::HdmiB);
        state.hotplug_enable |= SDVOC_HOTPLUG_INT_EN;
    }
    if (dpb & PORT_DETECTED) != 0 {
        state = state.push(Port::DpA);
        state.hotplug_enable |= PORTB_HOTPLUG_INT_EN;
    }
    if (dpc & PORT_DETECTED) != 0 {
        state = state.push(Port::DpB);
        state.hotplug_enable |= PORTC_HOTPLUG_INT_EN;
    }
    if (dpd & PORT_DETECTED) != 0 {
        state = state.push(Port::DpC);
        state.hotplug_enable |= PORTD_HOTPLUG_INT_EN;
    }
    state
}

/// Run legacy GMCH detection and program PORT_HOTPLUG_EN.
pub(crate) fn initialize_legacy_gmch(mmio: &Mmio, cpu: Cpu) -> LegacyPortDetectState {
    let state = legacy_gmch_detect_state(
        cpu,
        mmio.read32(crate::port::GMCH_HDMIB),
        mmio.read32(crate::port::GMCH_HDMIC),
        mmio.read32(crate::port::GMCH_DPB),
        mmio.read32(crate::port::GMCH_DPC),
        mmio.read32(crate::port::GMCH_DPD),
    );
    hotplug_regs(mmio).enable.set(state.hotplug_enable);
    state
}

/// Return and clear a pending libgfxinit-style hotplug status event.
pub(crate) fn hotplug_detect(mmio: &Mmio, port: Port) -> Result<bool, GmaError> {
    let mask = hotplug_status_mask(port)?;
    let regs = hotplug_regs(mmio);
    let status = regs.status.get();
    let detected = (status & mask) != 0;
    if detected {
        regs.status.set(mask);
        let _ = regs.status.get();
    }
    Ok(detected)
}

/// Clear a pending hotplug status event for `port` if the port has one.
pub(crate) fn clear_hotplug_detect(mmio: &Mmio, port: Port) -> Result<(), GmaError> {
    hotplug_detect(mmio, port).map(|_| ())
}

fn hotplug_regs(mmio: &Mmio) -> &'static GmchHotplugRegs {
    // SAFETY: callers pass the validated and decoded GMA display MMIO window;
    // PORT_HOTPLUG_EN/STAT are the adjacent legacy GMCH hotplug registers.
    unsafe { mmio.reg_block::<GmchHotplugRegs>(PORT_HOTPLUG_EN) }
}

const fn hotplug_status_mask(port: Port) -> Result<u32, GmaError> {
    match port {
        Port::DpA => Ok(HP_STAT::PORTB_HOTPLUG_STATUS.val(3).value),
        Port::DpB => Ok(HP_STAT::PORTC_HOTPLUG_STATUS.val(3).value),
        Port::DpC => Ok(HP_STAT::PORTD_HOTPLUG_STATUS.val(3).value),
        Port::HdmiA => Ok(HP_STAT::PORT_DETECTED::SET.value),
        Port::HdmiB => Ok(HP_STAT::SDVOC_HOTPLUG_STATUS::SET.value),
        Port::Vga => Ok(HP_STAT::CRT_HOTPLUG_STATUS::SET.value),
        Port::Lvds | Port::Edp | Port::HdmiC | Port::DpD => Err(GmaError::UnsupportedPort),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gm965_accepts_vga_and_lvds_without_digital_detect_bits() {
        let state = legacy_gmch_detect_state(Cpu::Gm965, 0, 0, 0, 0, 0);
        assert!(state.is_valid(Port::Vga));
        assert!(state.is_valid(Port::Lvds));
        assert!(!state.is_valid(Port::HdmiA));
        assert_eq!(
            state.hotplug_enable,
            CRT_HOTPLUG_INT_EN | CRT_HOTPLUG_ACTIVATION_PERIOD_64
        );
    }

    #[test]
    fn digital_detect_bits_enable_matching_hotplug_sources() {
        let state = legacy_gmch_detect_state(Cpu::G45, PORT_DETECTED, 0, PORT_DETECTED, 0, 0);
        assert!(state.is_valid(Port::Vga));
        assert!(!state.is_valid(Port::Lvds));
        assert!(state.is_valid(Port::HdmiA));
        assert!(state.is_valid(Port::DpA));
        assert_ne!(state.hotplug_enable & SDVOB_HOTPLUG_INT_EN, 0);
        assert_ne!(state.hotplug_enable & PORTB_HOTPLUG_INT_EN, 0);
    }

    #[test]
    fn hotplug_status_masks_match_libgfxinit_g45() {
        assert_eq!(hotplug_status_mask(Port::DpA), Ok(3 << 17));
        assert_eq!(hotplug_status_mask(Port::DpB), Ok(3 << 19));
        assert_eq!(hotplug_status_mask(Port::DpC), Ok(3 << 21));
        assert_eq!(hotplug_status_mask(Port::HdmiA), Ok(1 << 2));
        assert_eq!(hotplug_status_mask(Port::HdmiB), Ok(1 << 3));
        assert_eq!(hotplug_status_mask(Port::Vga), Ok(1 << 11));
    }
}
