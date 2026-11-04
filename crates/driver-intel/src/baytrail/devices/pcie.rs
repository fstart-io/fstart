//! PCIe root ports 1-4 (00:1c.0-3) (coreboot `pcie.c`).

use super::Function;
use fstart_pci::ecam::EcamDevice;

/// Root port policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieConfig {
    /// Ports the board wires; port 1 must stay on while any other is.
    pub ports: [bool; 4],
    /// Enable CLKREQ# on port 1.
    pub clkreq: bool,
}

impl PcieConfig {
    pub const fn new() -> Self {
        Self {
            ports: [true; 4],
            clkreq: false,
        }
    }
}

impl Default for PcieConfig {
    fn default() -> Self {
        Self::new()
    }
}

// Root port configuration registers.
const XCAP: u16 = 0x40;
const SI: u32 = 1 << 24;
const DCAP: u16 = 0x44;
const DCTL_DSTS: u16 = 0x48;
const LCAP: u16 = 0x4c;
const L1EXIT_SHIFT: u32 = 15;
const LCTL: u16 = 0x50;
const CCC: u16 = 1 << 6;
const RL: u16 = 1 << 5;
const LD: u32 = 1 << 4;
const SLCAP: u16 = 0x54;
const SLCTL_SLSTS: u16 = 0x58;
const PDS: u32 = 1 << 22;
const DCAP2: u16 = 0x64;
const DSTS2: u16 = 0x68;
const CHCFG: u16 = 0xd0;
const MPC2: u16 = 0xd4;
const MPC: u16 = 0xd8;
const RPPGEN: u16 = 0xe0;
const PWRCTL: u16 = 0xe8;
const PHYCTL2_IOSFBCTL: u16 = 0xf4;
const PLL_OFF_EN: u32 = 1 << 8;
const STRPFUSECFG: u16 = 0xfc;
const AERCH: u16 = 0x100;
const NFTS: u16 = 0x314;
const CFG2: u16 = 0x320;
const PCIEDBG: u16 = 0x324;
const PCIESTS1: u16 = 0x328;
const PCIEALC: u16 = 0x338;
const RTP: u16 = 0x33c;
const PHYCTL4: u16 = 0x408;
const SQDIS: u32 = 1 << 27;

fn port(n: u8) -> EcamDevice {
    Function::PciePort(n).device()
}

/// Whether the lane strapping leaves port `n` (1-4) enabled.
fn strapped(strpfusecfg: u32, n: u8) -> bool {
    let lanes = (strpfusecfg >> 14) & 3;
    match n {
        // 4x1 only.
        2 => lanes == 0,
        // Off only in 1x4.
        3 => lanes != 3,
        // Off in 1x4 and 2x2.
        4 => lanes < 2,
        _ => true,
    }
}

/// Mark the slot implemented and report whether a device is present.
fn slot_present(device: &EcamDevice) -> bool {
    device.or32(XCAP, SI);
    device.read32(SLCTL_SLSTS) & PDS != 0
}

/// Squelch an empty port (coreboot `no_dev_behind_port`).
fn quiesce(device: &EcamDevice) {
    device.or32(PCIEALC, 1 << 26);
    for _ in 0..5_000 {
        if device.read32(PCIESTS1) & 0x1f00_0000 == 1 << 24 {
            break;
        }
        fstart_arch::x86::udelay(10);
    }
    device.or32(PHYCTL4, SQDIS);
}

/// The port state enumeration and init need from the enable phase.
#[derive(Debug, Clone, Copy, Default)]
pub struct PcieState {
    /// PLL_OFF_EN of port 1 shifts every port's L1 exit latency.
    pub pll_off: bool,
    pub enabled: [bool; 4],
}

/// Before enumeration: drop strapped-off and empty ports, hide disabled
/// ones (coreboot `byt_pcie_enable`).
pub fn enable(config: &PcieConfig) -> PcieState {
    let mut state = PcieState::default();
    let first = port(1);
    if !first.is_present() {
        return state;
    }
    state.pll_off = first.read32(PHYCTL2_IOSFBCTL) & PLL_OFF_EN != 0;
    let strpfusecfg = first.read32(STRPFUSECFG);
    for n in 1..=4u8 {
        let device = port(n);
        if !device.is_present() {
            continue;
        }
        let mut enabled = config.ports[usize::from(n - 1)] && strapped(strpfusecfg, n);
        if !slot_present(&device) {
            fstart_log::debug!("baytrail: no PCIe device behind port {}", n);
            // Port 1 stays while another port has a device.
            let others_empty = n != 1
                || (2..=4).all(|other| {
                    let other = port(other);
                    !other.is_present() || !slot_present(&other)
                });
            if others_empty {
                quiesce(&device);
                enabled = false;
            }
        } else if !enabled {
            // Disabled port with a device: disable the link.
            device.or32(LCTL, LD);
        }
        state.enabled[usize::from(n - 1)] = enabled;
    }
    // Port 1 is function 0 of 1c: hiding it hides every other port.
    if !state.enabled[0] && state.enabled[1..].iter().any(|&enabled| enabled) {
        fstart_log::warn!("baytrail: keeping PCIe port 1 for the ports behind it");
        state.enabled[0] = true;
    }
    for n in 1..=4u8 {
        if port(n).is_present() {
            super::set_enabled(Function::PciePort(n), state.enabled[usize::from(n - 1)]);
        }
    }
    state
}

/// After allocation: static settings, exit latencies, slot capabilities
/// and clock gating (coreboot `byt_pcie_init`).
pub fn init(config: &PcieConfig, state: &PcieState) {
    for n in 1..=4u8 {
        let device = port(n);
        if !state.enabled[usize::from(n - 1)] || !device.is_present() {
            continue;
        }
        // No OBFF or LTR, 128-byte MPS, no flush timer or idle counters.
        device.and32(DCAP2, !(0x3 << 18 | 1 << 11));
        device.and32(DSTS2, !(3 << 13 | 1 << 10));
        device.and32(DCAP, !0x7);
        device.and32(PHYCTL2_IOSFBCTL, !(3 << 14 | 3 << 12 | 3 << 26));
        // L1 exit latency, longer when the PLL turns off in L1.
        let shift = L1EXIT_SHIFT + u32::from(state.pll_off);
        device.modify32(LCAP, !(0x7 << L1EXIT_SHIFT), 2 << shift);
        device.or16(LCTL, CCC);
        device.write32(NFTS, 0x743a_361b);
        device.modify32(MPC, !(0x7 << 15), 0x3 << 15);
        device.modify32(RTP, 0xff00_0000, 0x0085_4c74);
        // IOSF fast transmit, link speed training policy.
        device.or16(MPC2, 1 << 11 | 1 << 6);
        // Upstream posted split off, non-posted and posted request size.
        device.modify32(CHCFG, !(1 << 24), 1 << 15 | 1 << 14);
        // Completion replay, TLP grant count 3.
        device.modify32(CFG2, !(0x7 << 6), 1 << 22 | 3 << 6);
        // No IOAPIC behind the port: no EOI forwarding.
        device.or16(MPC2, 1 << 1);
        // Expose AER.
        device.or32(AERCH, 1 << 16 | 1 << 0);
        // Completion timeout 160-170 ms.
        device.write16(DSTS2, (device.read16(DSTS2) & !(1 << 4)) | 0x6);
        device.or16(DCTL_DSTS, 0xf);
        // Read back and rewrite the capability pointers.
        device.or32(0x34, 0);
        device.or32(0x80, 0);
        device.or16(LCTL, RL);
        // No hot plug, 10 W, slot number.
        device.modify32(
            SLCAP,
            !(1 << 6 | 1 << 5),
            1 << 15 | 100 << 7 | u32::from(n - 1) << 19,
        );
        // Dynamic clock gating.
        device.or32(RPPGEN, 1 << 9 | 1 << 8 | 1 << 15);
        device.or32(PWRCTL, 1 << 1 | 1 << 0);
        device.or32(PCIEDBG, 1 << 5);
        if n == 1 {
            let mut gating = 1 << 11 | 1 << 10;
            if config.clkreq {
                gating |= 1 << 13 | 1 << 12;
            }
            device.or32(RPPGEN, gating);
        }
    }
}

/// Wait up to 50 ms for the link of each enabled port before scanning
/// behind it.
pub fn wait_for_links(state: &PcieState) {
    for n in (1..=4u8).filter(|n| state.enabled[usize::from(n - 1)]) {
        let device = port(n);
        for _ in 0..5_000 {
            if device.read32(LCTL) & 1 << 29 != 0 {
                break;
            }
            fstart_arch::x86::udelay(10);
        }
    }
}
