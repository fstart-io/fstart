//! The four IOD SATA (AHCI) controllers of Rome and Milan (FCH_SP3 §3.1
//! af47, §3.2 af57, §3.3 af77).
//!
//! Controller `c` is at SMN `0310_0000h + 0x10_0000·c` (control registers;
//! AHCI ABAR at `+0x1000`), its PHY group gate at [`PHY_GATE`]\[c\], its NBIF
//! function straps at [`nbif`]. All accesses are on the socket's SMN, so the
//! same code serves socket 1.

use tock_registers::interfaces::{Readable, Writeable};

use super::super::gpio::{self, Direction, Pad, Pin, Pull};
use super::super::{FchHw, SmnWindow, Window, irq, poll, update};
use crate::sp3::nbio::nbif::func_strap0;
use crate::sp3::soc::Generation;

pub const CONTROLLERS: usize = 4;

/// Control block of controller `c`.
pub const fn base(c: usize) -> u32 {
    0x0310_0000 + 0x10_0000 * c as u32
}
/// AHCI registers (ABAR) of controller `c`.
pub const fn abar(c: usize) -> u32 {
    base(c) + 0x1000
}

/// PHY group gates of controllers 0..3 (bits 2:0 = power down; unnamed,
/// FCH_SP3 §10).
pub const PHY_GATE: [u32; CONTROLLERS] = [0x13C3_9404, 0x13C3_9804, 0x13D3_9404, 0x13D3_9804];
/// PHY groups without a SATA controller, powered down at reset.
pub const UNUSED_PHY: [u32; 4] = [0x13B3_9404, 0x13B3_9804, 0x13E3_9404, 0x13E3_9804];

/// NBIF function strap block of controller `c` (`+0x00` IDs, `+0x0C`
/// strap, `+0x34` class): NBIF1 devices 1/2 function 0 of quadrants 1
/// (controllers 0/1, `1063_5000h`/`1063_6000h`) and 2 (controller 2,
/// `1073_5000h`; FCH_SP3 §3.2 af57). Controller 3 at `1073_6000h` follows
/// the pattern (inferred).
pub const fn nbif(c: usize) -> u32 {
    let quadrant = [0x1060_0000, 0x1060_0000, 0x1070_0000, 0x1070_0000][c];
    quadrant + func_strap0(1 + (c % 2) as u8, 0)
}

/// Registers of the control block (offsets from [`base`]) and of the AHCI
/// block (from [`abar`]).
mod reg {
    pub const CONTROL: u32 = 0x0000;
    pub const PORT_CONTROL: u32 = 0x0004;
    pub const CTL_0008: u32 = 0x0008;
    // AHCI, relative to ABAR.
    pub const CAP: u32 = 0x000;
    pub const EM_CTL: u32 = 0x020;
    pub const GHC_24: u32 = 0x024;
    pub const PI: u32 = 0x00C;
    pub const DEVSLP: u32 = 0x0F4;
    pub const ESP_F8: u32 = 0x0F8;
    pub const ESP_FC: u32 = 0x0FC;
    pub const LOCK: u32 = 0x800;
    pub const MISC_804: u32 = 0x804;
    pub const MISC_810: u32 = 0x810;
    pub const MISC_814: u32 = 0x814;
    pub const PORT_MAP: u32 = 0x88C;
    pub const fn port(n: u32) -> u32 {
        0x100 + 0x80 * n
    }
    pub const PX_IS: u32 = 0x10;
    pub const PX_SSTS: u32 = 0x28;
    pub const PX_SCTL: u32 = 0x2C;
    pub const PX_SERR: u32 = 0x30;
}

/// EM_CTL\[8\] transmit message.
const EM_TM: u32 = 1 << 8;
/// SATA control unlock / lock bits (`ABAR+0x800`: bit 0 write enable,
/// bits 23:16 port disable).
const UNLOCK: u32 = 1;

/// One controller's policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controller {
    pub enabled: bool,
    /// Ports that are external/hot-plug (ESP); never disabled at late time.
    pub esp_ports: u8,
    /// SGPIO enclosure management with these pads (board policy).
    pub sgpio: Option<&'static [Pad]>,
}

/// The donor sets all IOMUX functions first, then the pulls.
const fn sgpio_pad(pin: u16, function: u8, pull: bool) -> Pad {
    Pad {
        pin: Pin(pin),
        function: if pull { None } else { Some(function) },
        pull: if pull { Some(Pull::Up) } else { None },
        direction: Direction::Keep,
    }
}

/// SGPIO pads of controller 0: 9/10/23/40 (FCH_SP3 §3.2 af57).
pub const SGPIO0_PADS: &[Pad] = &[
    sgpio_pad(9, 1, false),
    sgpio_pad(10, 2, false),
    sgpio_pad(23, 1, false),
    sgpio_pad(40, 1, false),
    sgpio_pad(9, 1, true),
    sgpio_pad(10, 2, true),
    sgpio_pad(23, 1, true),
];
/// SGPIO pads of controller 1: remote 260–263.
pub const SGPIO1_PADS: &[Pad] = &[
    sgpio_pad(260, 1, false),
    sgpio_pad(261, 1, false),
    sgpio_pad(262, 1, false),
    sgpio_pad(263, 1, false),
    sgpio_pad(260, 1, true),
    sgpio_pad(261, 1, true),
    sgpio_pad(262, 1, true),
];

/// SATA policy of one socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SataPolicy {
    pub controllers: [Controller; CONTROLLERS],
}

impl SataPolicy {
    /// The S8030 donor defaults on socket 0 (Rome and Milan DXE): 0..2 on
    /// (0 and 1 with SGPIO, ESP 3Fh/FFh), 3 off. The Milan PEI also takes
    /// controller 3 out of reset before DXE turns it off.
    pub const fn socket0() -> Self {
        Self {
            controllers: [
                Controller {
                    enabled: true,
                    esp_ports: 0x3F,
                    sgpio: Some(SGPIO0_PADS),
                },
                Controller {
                    enabled: true,
                    esp_ports: 0xFF,
                    sgpio: Some(SGPIO1_PADS),
                },
                Controller {
                    enabled: true,
                    esp_ports: 0,
                    sgpio: None,
                },
                Self::OFF,
            ],
        }
    }

    /// Socket 1 at the donor defaults: all controllers off (FCH_SP3 §4.2).
    pub const fn socket1() -> Self {
        Self {
            controllers: [Self::OFF; CONTROLLERS],
        }
    }

    const OFF: Controller = Controller {
        enabled: false,
        esp_ports: 0,
        sgpio: None,
    };
}

/// PEI af47: power down the unused PHY groups, take enabled controllers out
/// of reset and ungate their PHYs. The donors never clear the gate of
/// controller 3 here (Milan clears its control bit only); kept as recovered.
pub fn reset<H: FchHw>(hw: &H, policy: &SataPolicy) {
    let smn = hw.smn_window();
    UNUSED_PHY
        .iter()
        .for_each(|&r| update::<u32, _>(&smn, r, 0, 7));
    policy
        .controllers
        .iter()
        .enumerate()
        .filter(|(_, c)| c.enabled)
        .for_each(|(c, _)| {
            update::<u32, _>(&smn, base(c) + reg::CONTROL, 1, 0);
            if c < 3 {
                update::<u32, _>(&smn, PHY_GATE[c], 7, 0);
            }
        });
}

/// Put controller `c` in reset and power its PHY down.
pub fn disable<H: FchHw>(hw: &H, generation: Generation, c: usize) {
    let smn = hw.smn_window();
    let clear = if generation == Generation::Milan {
        0x0100_000E
    } else {
        0
    };
    update::<u32, _>(&smn, base(c) + reg::CONTROL, clear, 0x1);
    update::<u32, _>(&smn, base(c) + reg::CONTROL, clear, 0x801);
    update::<u32, _>(&smn, PHY_GATE[c], 0, 7);
}

/// Clear the port control bits 0..7 one by one, as the donor does.
fn clear_port_control<W: Window>(smn: &W, c: usize) {
    (0..8).for_each(|i| update::<u32, _>(smn, base(c) + reg::PORT_CONTROL, (2 << i) - 1, 0));
}

/// Program the ESP (external port) masks.
fn esp<W: Window>(smn: &W, generation: Generation, c: usize, ports: u8) {
    let a = abar(c);
    let esp = u32::from(ports);
    let r810 = match generation {
        Generation::Milan => (0x4300, 0x0200_8004),
        _ => (0, 0x0200_C304),
    };
    if esp != 0 {
        update::<u32, _>(smn, a + reg::ESP_F8, esp, 0xFF00_0000);
        update::<u32, _>(
            smn,
            a + reg::ESP_F8,
            (!esp & 0xff) << 16 | esp,
            esp << 16 | 0xFF00_0000,
        );
        update::<u32, _>(smn, a + reg::ESP_FC, 0x0808_0000, 0x0410_0C02);
        update::<u32, _>(smn, a + reg::MISC_810, r810.0, r810.1 | 1 << 29);
    } else {
        update::<u32, _>(smn, a + reg::ESP_F8, 0x00FF_00FF, 0xFF00_0000);
        update::<u32, _>(smn, a + reg::ESP_FC, 0x0818_0000, 0x0400_0C02);
        update::<u32, _>(smn, a + reg::MISC_810, r810.0 | 1 << 29, r810.1);
    }
}

/// Send the SGPIO setup through the AHCI enclosure-management buffer. The
/// donor waits for EM_CTL.TM to clear without a bound; here each message
/// waits at most 10 ms and a timeout stops the sequence.
fn sgpio<H: FchHw>(hw: &H, smn: &SmnWindow<H::Pci>, c: usize, pads: &[Pad]) -> bool {
    gpio::configure_all(hw.acpi(), pads);
    update::<u32, _>(smn, base(c) + reg::CONTROL, 0x0100_0007, 0x8);
    update::<u32, _>(smn, abar(c) + reg::ESP_FC, 0x0008_0000, 0x0C10_0C02);
    super::super::tables::SGPIO_MESSAGES.iter().all(|message| {
        message.iter().for_each(|op| op.at(abar(c)).apply(smn));
        let em = smn.reg::<u32, ()>(abar(c) + reg::EM_CTL);
        em.set(em.get() | EM_TM);
        poll(hw.timer(), 1000, 10, || em.get() & EM_TM == 0)
    })
}

/// DXE af57 on one socket. Returns `false` if an SGPIO message timed out
/// (the controller is still usable).
pub fn env<H: FchHw>(hw: &H, generation: Generation, policy: &SataPolicy) -> bool {
    let smn = hw.smn_window();
    let mut sgpio_ok = true;
    for (c, ctl) in policy.controllers.iter().enumerate() {
        if !ctl.enabled {
            disable(hw, generation, c);
            continue;
        }
        let a = abar(c);
        clear_port_control(&smn, c);
        update::<u32, _>(&smn, a + reg::LOCK, 0, UNLOCK);
        [0x5, 0xD, 0xF, 0x0100_000F]
            .into_iter()
            .for_each(|clear| update::<u32, _>(&smn, base(c) + reg::CONTROL, clear, 0));
        update::<u32, _>(&smn, a + reg::ESP_FC, 0x0808_0000, 0x0400_0C02);
        update::<u32, _>(&smn, a + reg::ESP_F8, 0, 0xFF00_0000);
        update::<u32, _>(&smn, a + reg::GHC_24, 1, 0);
        match generation {
            Generation::Milan => update::<u32, _>(&smn, a + reg::MISC_810, 0x4300, 0x0200_8004),
            _ => update::<u32, _>(&smn, a + reg::MISC_810, 0, 0x0200_C304),
        }
        update::<u32, _>(&smn, a + reg::MISC_814, 0, 0x4000_01E0);
        update::<u32, _>(&smn, a + reg::LOCK, 0, 0x2001);
        update::<u32, _>(&smn, nbif(c) + 0x0C, 0, 0x4_0000);
        update::<u32, _>(&smn, a + reg::LOCK, 0xFF_0000, 0x2001);
        update::<u32, _>(&smn, a + reg::MISC_804, 0, 0x40_0000);
        esp(&smn, generation, c, ctl.esp_ports);
        // Clear SError and pending interrupts of all eight ports.
        (0..8).for_each(|n| {
            let p = a + reg::port(n);
            smn.write32(p + reg::PX_SERR, 0xFFFF_FFFF);
            let is = smn.reg::<u32, ()>(p + reg::PX_IS);
            is.set(is.get());
        });
        if let Some(pads) = ctl.sgpio {
            sgpio_ok &= sgpio(hw, &smn, c, pads);
        }
        if c == 0 {
            // Controller 0 only: DevSleep pads 5/6 back to GPIO, DevSlp off,
            // and the identity port map (ABAR+0x88C + 0x20n [10:8] = n).
            hw.acpi().write8(super::super::acpimmio::IOMUX + 5, 0);
            update::<u32, _>(&smn, a + reg::DEVSLP, 0x100, 0);
            update::<u32, _>(&smn, a + reg::DEVSLP, 0x110, 0);
            hw.acpi().write8(super::super::acpimmio::IOMUX + 6, 0);
            update::<u32, _>(&smn, a + reg::DEVSLP, 0x310, 0);
            update::<u32, _>(&smn, a + reg::DEVSLP, 0x310, 0);
            (0..8)
                .for_each(|n| update::<u32, _>(&smn, a + reg::PORT_MAP + 0x20 * n, 0x700, n << 8));
        }
        // AHCI class code and device ID straps, then lock.
        smn.write32(nbif(c) + 0x34, 0x0001_0601);
        update::<u32, _>(&smn, nbif(c), 0x86FE, 0x7901);
        update::<u32, _>(&smn, a + reg::LOCK, 0xFF_0001, 0x2000);
    }
    irq::update(hw.io(), irq::INTR_08, 0, 0xF0);
    sgpio_ok
}

/// DXE late af77 (also SW SMI b4): disable non-ESP ports without a device,
/// shrink PI/CAP.NP accordingly, lock.
pub fn late<H: FchHw>(hw: &H, policy: &SataPolicy) {
    let smn = hw.smn_window();
    for (c, ctl) in policy
        .controllers
        .iter()
        .enumerate()
        .filter(|(_, c)| c.enabled)
    {
        let a = abar(c);
        update::<u32, _>(&smn, a + reg::LOCK, 0xFF_0000, 0x2001);
        let mut disabled = 0u32;
        for n in (0..8).filter(|n| ctl.esp_ports & (1 << n) == 0) {
            let p = a + reg::port(n);
            if smn.read32(p + reg::PX_SSTS) & 0xF == 3 {
                continue;
            }
            update::<u32, _>(&smn, p + reg::PX_SCTL, 0xB, 0x4);
            disabled |= 1 << n;
            update::<u32, _>(&smn, a + reg::LOCK, 0xFF_0000, disabled << 16 | 0x2001);
            update::<u32, _>(&smn, a + reg::PI, disabled, 0);
        }
        let pi = smn.reg::<u32, ()>(a + reg::PI);
        if pi.get() & 0xFF == 0 {
            update::<u32, _>(&smn, a + reg::PI, 0xFE, 1);
        }
        let ports = (pi.get() & 0xFF).count_ones().max(1);
        update::<u32, _>(&smn, a + reg::CAP, 0x1F, ports - 1);
        // Control 0x0008: the donor writes back what it reads.
        let r = smn.reg::<u32, ()>(base(c) + reg::CTL_0008);
        r.set(r.get());
        update::<u32, _>(&smn, a + reg::LOCK, UNLOCK, 0x2000);
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::model::{fch, smn, smn_writes};
    use super::*;

    #[test]
    fn reset_rome_default() {
        let hw = fch();
        reset(&hw, &SataPolicy::socket0());
        let w = smn_writes(&hw.pci, 0);
        let addrs: Vec<u32> = w.iter().map(|w| w.0).collect();
        // FCH_SP3 §3.1 af47 SSP order (gate writes may repeat).
        assert!(addrs.starts_with(&UNUSED_PHY));
        assert!(!addrs.contains(&0x0340_0000));
        assert!(!addrs.contains(&0x13d3_9804));
        assert_eq!(smn(&hw.pci, 0, 0x13b3_9404), 7);
    }

    #[test]
    fn env_and_late_rome_controller2() {
        let hw = fch();
        // Ports 0 and 3 of controller 2 have a device.
        [0, 3].into_iter().for_each(|n| {
            hw.pci
                .0
                .borrow_mut()
                .smn
                .insert((0, abar(2) + reg::port(n) + reg::PX_SSTS), 0x123);
        });
        hw.pci
            .0
            .borrow_mut()
            .smn
            .insert((0, abar(2) + reg::PI), 0xff);
        hw.pci
            .0
            .borrow_mut()
            .smn
            .insert((0, abar(2) + reg::CAP), 0x1f);
        let mut policy = SataPolicy::socket0();
        policy.controllers[0].enabled = false;
        policy.controllers[1].enabled = false;
        assert!(env(&hw, Generation::Rome, &policy));
        assert_eq!(smn(&hw.pci, 0, nbif(2) + 0x34), 0x10601);
        assert_eq!(smn(&hw.pci, 0, abar(2) + reg::ESP_F8), 0xff00_0000);
        assert_eq!(smn(&hw.pci, 0, base(3)), 0x801);
        assert_eq!(smn(&hw.pci, 0, PHY_GATE[3]), 7);
        late(&hw, &policy);
        assert_eq!(smn(&hw.pci, 0, abar(2) + reg::PI), 0x09);
        assert_eq!(smn(&hw.pci, 0, abar(2) + reg::CAP), 0x01);
        assert_eq!(smn(&hw.pci, 0, abar(2) + reg::LOCK) & 0xff_0001, 0xf6_0000);
    }

    #[test]
    fn sgpio_messages_wait_for_tm() {
        let hw = fch();
        let mut policy = SataPolicy::socket0();
        policy.controllers[1].enabled = false;
        policy.controllers[2].enabled = false;
        // EM_CTL never clears TM in the model: the first message times out.
        assert!(!env(&hw, Generation::Milan, &policy));
        assert_eq!(hw.acpi.get(0xd0a, 1), 2);
        assert_eq!(smn(&hw.pci, 0, abar(0) + 0x50c), 0x20);
        assert_eq!(smn(&hw.pci, 0, abar(0) + reg::MISC_810), 0x2200_8004);
    }
}
