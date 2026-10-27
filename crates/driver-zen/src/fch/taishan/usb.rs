//! The per-die ZP USB 3 controller, programmed directly over SMN (FCH_SP3
//! §3.1 af48, §3.4, §4.1, §5).

use super::super::{FchHw, Window, hide_nbif_function, poll, update};
use crate::nbio::nbif::func_strap0;

/// Controller control/status block.
const CTL_0014: u32 = 0x16D8_0014;
const RESET_CTL: u32 = 0x16D8_0100;
const SX_CTL: u32 = 0x16D8_0104;
const SX_STATUS: u32 = 0x16D8_0108;
const CTL_0118: u32 = 0x16D8_0118;
/// Port disable and OC polarity: the SP3 package disables ports 2–3.
const PORT_DISABLE: u32 = 0x16D8_0128;
const OC_POLARITY: u32 = 0x16D8_0130;
const CTL_014C: u32 = 0x16D8_014C;
/// NBIF0 device 0 function 3 strap: the die's xHCI function (`1013_4600h`).
const NBIF_STRAP: u32 = 0x1010_0000 + func_strap0(0, 3);
/// Handshake with the SMU around S3 (unnamed SMN `0004D520`/`0004D528`).
const SMU_REQ: u32 = 0x0004_D520;
const SMU_ACK: u32 = 0x0004_D528;

/// What a die's USB controller does (FCH_SP3 §4.1 defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DieUsb {
    /// Function strapped off only (2P dies 4/5 at the donor default).
    Off,
    /// Strapped off and held in reset (dies 2, 3, 6, 7).
    HeldInReset,
    /// Initialized, with this PHY parameter (`16D0C000h` per port).
    Enabled { phy: u32 },
}

impl DieUsb {
    /// The Naples donor default of die `die` (0..7, socket 1 = 4..7).
    pub const fn default_for(die: u8) -> Self {
        match die {
            0 => Self::Enabled { phy: 0x01B3_C953 },
            1 => Self::Enabled { phy: 0x01B3_CD56 },
            4 | 5 => Self::Off,
            _ => Self::HeldInReset,
        }
    }
}

const PORTS: u32 = 4;
const fn port(base: u32, n: u32) -> u32 {
    base + 0x400 * n
}

/// The post-reset DWC3/PHY settings, shared by cold boot and S3 resume.
fn post_reset<W: Window>(smn: &W) {
    smn.write32(0x16C0_C11C, 0x2004_010A);
    smn.write32(0x16C0_C12C, 0x0A43_487D);
    (0..4).for_each(|i| smn.write32(0x16C0_C2C0 + 4 * i, 0x010A_0002));
    (0..PORTS).for_each(|n| update::<u32, _>(smn, port(0x16D0_4018, n), 0x440, 0xB80));
    (0..PORTS).for_each(|n| smn.write32(port(0x16D0_4098, n), 1));
}

/// One request/acknowledge step of the SMU handshake: write `request`, then
/// repeatedly post 107h and wait for `ack & 3 == want` (1000 × 1 µs, as the
/// donor; a timeout continues like the donor does).
fn handshake<H: FchHw, W: Window>(hw: &H, smn: &W, request: u32, want: u32) -> bool {
    smn.write32(SMU_REQ, request);
    poll(hw.timer(), 1000, 1, || {
        smn.write32(SMU_REQ, 0x107);
        smn.read32(SMU_ACK) & 3 == want
    })
}

/// PEI af48 for the die of `hw`. `resume` is the S3 path.
pub fn reset<H: FchHw>(hw: &H, usb: DieUsb, resume: bool) {
    let smn = hw.smn_window();
    update::<u32, _>(&smn, CTL_014C, 0, 1 << 31);
    let phy = match usb {
        DieUsb::Off => {
            hide_nbif_function(&smn, NBIF_STRAP);
            return;
        }
        DieUsb::HeldInReset => {
            hide_nbif_function(&smn, NBIF_STRAP);
            update::<u32, _>(&smn, RESET_CTL, 0, 7);
            return;
        }
        DieUsb::Enabled { phy } => phy,
    };
    if resume {
        update::<u32, _>(&smn, RESET_CTL, 1, 0);
        handshake(hw, &smn, 0x101, 2);
        smn.write32(SX_CTL, 0x33);
        handshake(hw, &smn, 0x103, 0);
        smn.write32(CTL_0014, 0);
        post_reset(&smn);
        return;
    }
    smn.write32(CTL_0014, 0);
    update::<u32, _>(&smn, RESET_CTL, 0, 1);
    update::<u32, _>(&smn, RESET_CTL, 0, 7);
    smn.write32(OC_POLARITY, 0x22);
    smn.write32(PORT_DISABLE, 0x000C_000C);
    (0..PORTS).for_each(|n| smn.write32(port(0x16D0_8200, n), 0x39));
    (0..PORTS).for_each(|n| smn.write32(port(0x16D0_C000, n), phy));
    (0..PORTS).for_each(|n| update::<u32, _>(&smn, port(0x16D0_8008, n), 0x280, 0x100));
    update::<u32, _>(&smn, CTL_0118, 0x1000, 0);
    // Release the resets in the donor's three steps.
    update::<u32, _>(&smn, RESET_CTL, 1, 6);
    update::<u32, _>(&smn, RESET_CTL, 5, 2);
    update::<u32, _>(&smn, RESET_CTL, 7, 0);
    post_reset(&smn);
}

/// The S3 entry handshake of one die's controller (FchTaishanSmmInit Sx
/// path): wait for `16D80108[9:8]` = 3, request 101h, wait for the SMU,
/// then `16D80104` = C8h. Returns whether both waits completed; the donor
/// continues either way.
pub fn sx_handshake<H: FchHw>(hw: &H, bus: u8) -> bool {
    // SAFETY: SMM owns the SMN pair while it runs.
    let smn =
        unsafe { super::super::SmnWindow::new(crate::smn::Smn::new(hw.pci().clone(), bus), 0) };
    let idle = poll(hw.timer(), 1000, 1, || {
        smn.read32(SX_STATUS) & 0x300 == 0x300
    });
    let acked = handshake(hw, &smn, 0x101, 2);
    smn.write32(SX_CTL, 200);
    idle && acked
}

#[cfg(test)]
mod tests {
    use super::super::super::model::{fch, smn, smn_writes};
    use super::*;

    #[test]
    fn cold_die1() {
        let mut hw = fch();
        hw.bus = 0x20;
        reset(&hw, DieUsb::default_for(1), false);
        assert_eq!(smn(&hw.pci, 0x20, 0x16d0_cc00), 0x01b3_cd56);
        assert_eq!(smn(&hw.pci, 0x20, PORT_DISABLE), 0x000c_000c);
        assert_eq!(smn(&hw.pci, 0x20, RESET_CTL), 0);
        assert_eq!(smn(&hw.pci, 0x20, 0x16d0_4c18), 0xb80);
    }

    #[test]
    fn held_in_reset_die() {
        let mut hw = fch();
        hw.bus = 0x40;
        reset(&hw, DieUsb::default_for(2), false);
        let w = smn_writes(&hw.pci, 0x40);
        assert_eq!(
            w,
            vec![(CTL_014C, 1 << 31), (NBIF_STRAP, 0), (RESET_CTL, 7)]
        );
    }

    #[test]
    fn s3_handshake_bounded() {
        let hw = fch();
        assert!(!sx_handshake(&hw, 0));
        assert_eq!(smn(&hw.pci, 0, SX_CTL), 200);
        hw.pci.0.borrow_mut().smn.insert((0, SX_STATUS), 0x300);
        hw.pci.0.borrow_mut().smn.insert((0, SMU_ACK), 2);
        assert!(sx_handshake(&hw, 0));
    }
}
