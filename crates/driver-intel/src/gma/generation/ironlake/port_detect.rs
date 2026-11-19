//! PCH port presence and hotplug (libgfxinit `ironlake/hw-gfx-gma-port_detect`).

use super::{DP_CTL_A, PchDigital};
use crate::gma::mmio::Mmio;
use crate::gma::port_detect::PortDetectState;
use crate::gma::regs::{CPU_DP_CTL, PCH_DP, PCH_HDMI, PCH_LVDS, SHOTPLUG_CTL};
use crate::gma::types::Port;

const SHOTPLUG_CTL_REG: usize = 0xc4030;
const PCH_LVDS_REG: usize = 0xe1180;
const DIGITAL: [PchDigital; 3] = [PchDigital::B, PchDigital::C, PchDigital::D];

/// `SHOTPLUG_CTL_DETECT_MASK`: the write-1-to-clear status of all ports.
const DETECT_MASK: u32 = SHOTPLUG_CTL::PORTB_HPD_STATUS.val(3).value
    | SHOTPLUG_CTL::PORTC_HPD_STATUS.val(3).value
    | SHOTPLUG_CTL::PORTD_HPD_STATUS.val(3).value;

const fn shift(port: PchDigital) -> usize {
    8 * port.index()
}

const fn hpd_input_enable(port: PchDigital) -> u32 {
    SHOTPLUG_CTL::PORTB_HPD_ENABLE::SET.value << shift(port)
}

const fn short_pulse_mask(port: PchDigital) -> u32 {
    SHOTPLUG_CTL::PORTB_SHORT_PULSE.val(3).value << shift(port)
}

const fn hpd_status(port: PchDigital) -> u32 {
    SHOTPLUG_CTL::PORTB_HPD_STATUS.val(3).value << shift(port)
}

/// The long-pulse bit of a port's status.
const fn long_detect(port: PchDigital) -> u32 {
    SHOTPLUG_CTL::PORTB_HPD_STATUS.val(2).value << shift(port)
}

const fn hdmi_reg(port: PchDigital) -> usize {
    0xe1140 + 0x10 * port.index()
}

const fn dp_reg(port: PchDigital) -> usize {
    0xe4100 + 0x100 * port.index()
}

const fn hdmi_port(port: PchDigital) -> Port {
    match port {
        PchDigital::B => Port::HdmiA,
        PchDigital::C => Port::HdmiB,
        PchDigital::D => Port::HdmiC,
    }
}

const fn dp_port(port: PchDigital) -> Port {
    match port {
        PchDigital::B => Port::DpA,
        PchDigital::C => Port::DpB,
        PchDigital::D => Port::DpC,
    }
}

/// The PCH digital port carrying `port`'s hotplug, if any.
const fn digital(port: Port) -> Option<PchDigital> {
    match port {
        Port::HdmiA | Port::DpA => Some(PchDigital::B),
        Port::HdmiB | Port::DpB => Some(PchDigital::C),
        Port::HdmiC | Port::DpC => Some(PchDigital::D),
        _ => None,
    }
}

/// libgfxinit `Port_Detect.Initialize`: VGA always, the others by their
/// presence straps. With `initialize`, the hotplug input of detected digital
/// ports is enabled and pending events cleared; undetected ports are
/// disabled.
pub(crate) fn detect(mmio: &Mmio, initialize: bool) -> PortDetectState {
    let mut state = PortDetectState::empty().push(Port::Vga);
    if mmio.read32(PCH_LVDS_REG) & PCH_LVDS::PORT_DETECTED::SET.value != 0 {
        state = state.push(Port::Lvds);
    }
    if mmio.read32(DP_CTL_A) & CPU_DP_CTL::PORT_DETECTED::SET.value != 0 {
        state = state.push(Port::Edp);
    }
    for port in DIGITAL {
        let hdmi = mmio.read32(hdmi_reg(port)) & PCH_HDMI::PORT_DETECTED::SET.value != 0;
        let dp = mmio.read32(dp_reg(port)) & PCH_DP::PORT_DETECTED::SET.value != 0;
        if hdmi {
            state = state.push(hdmi_port(port));
        }
        if dp {
            state = state.push(dp_port(port));
        }
        if !initialize {
            continue;
        }
        if hdmi || dp {
            mmio.update32(
                SHOTPLUG_CTL_REG,
                DETECT_MASK | short_pulse_mask(port),
                hpd_input_enable(port) | hpd_status(port),
            );
        } else {
            mmio.update32(SHOTPLUG_CTL_REG, DETECT_MASK | hpd_input_enable(port), 0);
        }
    }
    state
}

/// libgfxinit `Port_Detect.Hotplug_Detect`: report a long pulse on the
/// port's pins and clear its pending status. Other ports never report.
pub(crate) fn hotplug_detect(mmio: &Mmio, port: Port) -> bool {
    let Some(port) = digital(port) else {
        return false;
    };
    let ctl = mmio.read32(SHOTPLUG_CTL_REG);
    if ctl & hpd_status(port) != 0 {
        mmio.update32(SHOTPLUG_CTL_REG, DETECT_MASK, hpd_status(port));
    }
    ctl & long_detect(port) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shotplug_fields_match_libgfxinit() {
        assert_eq!(DETECT_MASK, 0x0003_0303);
        assert_eq!(hpd_input_enable(PchDigital::C), 1 << 12);
        assert_eq!(short_pulse_mask(PchDigital::D), 3 << 18);
        assert_eq!(long_detect(PchDigital::B), 1 << 1);
        assert_eq!(long_detect(PchDigital::D), 1 << 17);
        assert_eq!(digital(Port::DpB), Some(PchDigital::C));
        assert_eq!(digital(Port::Edp), None);
    }

    #[test]
    fn straps_select_the_valid_ports() {
        let mut regs = alloc::vec![0u32; 0x10_0000 / 4];
        let set = |regs: &mut alloc::vec::Vec<u32>, offset: usize, value: u32| {
            regs[offset / 4] = value;
        };
        set(&mut regs, PCH_LVDS_REG, 1 << 1);
        set(&mut regs, dp_reg(PchDigital::C), 1 << 2);
        set(&mut regs, SHOTPLUG_CTL_REG, 0x0000_0303);
        // SAFETY: the backing vector covers every register touched here.
        let mmio = unsafe { Mmio::new(fstart_core::typed::mmio32(regs.as_mut_ptr() as u64)) };
        let state = detect(&mmio, true);
        assert!(state.is_valid(Port::Vga));
        assert!(state.is_valid(Port::Lvds));
        assert!(state.is_valid(Port::DpB));
        assert!(!state.is_valid(Port::HdmiB));
        assert!(!state.is_valid(Port::Edp));
        // Port C enabled and its status written back; port B disabled.
        let shotplug = regs[SHOTPLUG_CTL_REG / 4];
        assert_eq!(shotplug & (1 << 12), 1 << 12);
        assert_eq!(shotplug & 0x30, 0);
    }
}
