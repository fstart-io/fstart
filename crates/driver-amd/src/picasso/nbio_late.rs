// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native PCIe power/CLKREQ and late SMU notifications.
use crate::fam17h::tsc::DelayNs;
use super::{
    cpu::{Identity, Silicon},
    dxio::{Aspm, ClockRequest, Hotplug, Topology},
};
use crate::smu::v10::{self as smu, power};
use crate::smn::{Smn, SmnReg, apply, rmw, write};
use crate::fam17h::nbio::{IohcBridgeCntl, iohc_bridge_cntl, pcie};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// IOHC internal bridge 29.
const INTERNAL_BRIDGE: usize = 29;
const CHIP_ID: [SmnReg; 2] = [SmnReg::new(0x0005_d5bc), SmnReg::new(0x0005_d5b8)];

/// Return a permutation of eight physical CLKREQ pins. Trained L1-substate
/// ports request pins; if shared, the lower hardware port wins, as in native C.
pub fn clock_map(topology: &Topology) -> u32 {
    let mut requested = [u8::MAX; 8];
    let mut clocks = [0u8, 1, 2, 3, 4, 5, 6, 7];
    for (i, port) in topology.ports.iter().enumerate().rev() {
        let Some(d) = port.descriptor else {
            continue;
        };
        let clock = d.clock_request as u8;
        if port.trained
            && (d.l1_1 || d.l1_2)
            && (ClockRequest::Pin0 as u8..=ClockRequest::Pin7 as u8).contains(&clock)
        {
            requested[usize::from(clock - ClockRequest::Pin0 as u8)] = i as u8;
        }
    }
    for port in 0..8 {
        for pin in 0..8 {
            if requested[pin] != port as u8 {
                continue;
            }
            clocks.swap(port, pin);
            requested[pin] = requested[port];
            requested[port] = u8::MAX;
        }
    }
    clocks
        .into_iter()
        .enumerate()
        .fold(0, |map, (i, pin)| map | u32::from(pin) << (i * 4))
}
/// The vendor clock gating values of the core: raw.
fn core_gating() -> impl Iterator<Item = crate::smn::Reg> {
    [
        rmw(
            pcie::cpm_control(0),
            (1 << 16) | (3 << 9) | (3 << 12),
            1 | (1 << 9) | 2 | 4 | (1 << 6) | (1 << 12) | (1 << 14) | (1 << 24) | (1 << 25),
        ),
        rmw(pcie::core_raw(0, 0x044), 15 << 20, (1 << 20) | (1 << 21)),
        rmw(pcie::core_raw(0, 0x18c), 0, 0x4060),
    ]
    .into_iter()
    .chain((0..7).map(|i| rmw(pcie::port_raw(i, 0x1a8), 0, 1)))
}

pub fn port_power_management(smn: &Smn<impl ConfigRegionAccess>, topology: &Topology) {
    apply(smn, core_gating());
    let pll_off_in_l1 = |i| {
        smn.reg(pcie::lc_cntl6(i))
            .modify(pcie::LcCntl6::LC_L1_POWERDOWN::SET)
    };
    for (i, p) in topology.ports.iter().enumerate().rev() {
        if p.descriptor
            .is_some_and(|d| matches!(d.aspm, Aspm::L1 | Aspm::L0sL1))
        {
            pll_off_in_l1(i);
        }
    }
    for (i, p) in topology.ports.iter().enumerate().rev() {
        let Some(d) = p.descriptor else {
            continue;
        };
        if !d.l1_1 && !d.l1_2 {
            continue;
        }
        if p.trained {
            pll_off_in_l1(i);
            if d.l1_2 {
                apply(smn, (0..8).map(|j| rmw(pcie::port_raw(j, 0x304), 0, 1 << 5)));
            }
        } else if d.hotplug == Hotplug::Disabled {
            apply(smn, [rmw(0x1110_0378 + i as u32 * 0x1000, 15, 15)]);
            pll_off_in_l1(i);
        }
    }
    // The CLKREQ pin of each port, and the FCH MISC clock settings of the
    // vendor code through the SMN alias of the ACPI MMIO.
    apply(
        smn,
        [
            write(pcie::core_raw(0, 0xf4), clock_map(topology)),
            rmw(0x02d0_1eb0, 1 << 8, 0),
            rmw(0x02d0_1eb0, 1 << 24, 0),
            rmw(0x02d0_1eb4, 1 << 25, 0),
            rmw(0x02d0_1eb4, 1 << 24, 0),
        ],
    );
}
/// # Safety
/// Supply only topology returned by successful native training and permanently
/// reserved DMA pages meeting power::install's contract. Hardware changes are
/// fail-stop, not rollbackable; never call on a partially failed training flow.
pub unsafe fn pre_enumeration(
    smn: &Smn<impl ConfigRegionAccess>,
    topology: &Topology,
    pages: &'static mut power::Pages,
    config: power::Config,
    delay: &mut impl DelayNs,
    notice: &mut impl FnMut(u32, u32),
) -> Result<(), power::Error> {
    port_power_management(smn, topology);
    apply(smn, [rmw(0x11a0_b7c0, 3, 1)]);
    unsafe { power::install(smn, pages, config, delay, notice) }
}
/// Notify SMU only after all requested CPUs reached native initialization.
/// Caller must retain nonstandard-response diagnostics rather than claiming
/// their requested features were verified active.
pub fn ccx_done(
    smn: &Smn<impl ConfigRegionAccess>,
    graphics: bool,
    delay: &mut impl DelayNs,
    notice: &mut impl FnMut(u32, u32),
) -> Result<(), power::Error> {
    let [high, low] = CHIP_ID.map(|reg| smn.reg(reg).get());
    let chip_id = ((high << 5) | (low >> 27)) & 0xffff;
    if chip_id == 0x62e {
        power::command(smn, 0x35, [14, 0, 0, 0, 0, 0], delay, notice)?;
    }
    if chip_id == 0x633 {
        power::command(smn, 0x35, [8, 0, 0, 0, 0, 0], delay, notice)?;
    }
    let features = 0xf7fffffd & if graphics { u32::MAX } else { !(1 << 20) };
    power::command(smn, 5, [features, 0x8f7, 0, 0, 0, 0], delay, notice)?;
    power::command(smn, 0x0e, [0; 6], delay, notice)?;
    if !graphics {
        power::command(smn, 0x53, [0; 6], delay, notice)?;
    }
    for (message, first, second) in [(0x3a, 184, 0), (0x42, 0, 0), (0x32, 0, 0), (5, 0, 1 << 8)] {
        power::command(smn, message, [first, second, 0, 0, 0, 0], delay, notice)?;
    }
    Ok(())
}
/// Final step AFTER native graphics setup. The last RV2/PCO request is strict,
/// unlike the specifically documented late-policy nonstandard responses.
pub fn post_silicon(
    smn: &Smn<impl ConfigRegionAccess>,
    identity: Identity,
    delay: &mut impl DelayNs,
) -> Result<(), smu::Error> {
    smn.reg(iohc_bridge_cntl(INTERNAL_BRIDGE))
        .modify(IohcBridgeCntl::BRIDGE_DIS::SET + IohcBridgeCntl::CFG_DIS::SET);
    // NBIF0 function 8 strap off, SATA controller off.
    apply(
        smn,
        [
            rmw(0x1013_5000, 1 << 28, 0),
            rmw(0x0310_0000, 0, 1),
            rmw(0x0310_0000, 0, 1 << 11),
        ],
    );
    if matches!(identity.silicon, Silicon::Picasso | Silicon::Raven2) {
        smu::request(smn, 0x11, [0; 6], delay)?;
    }
    Ok(())
}
/// `devices` must contain enabled enumerated PCI functions. Retains the native
/// 1c5c quirk's function-number SMN index; do not substitute a devfn remap index.
pub fn after_enumeration(
    smn: &Smn<impl ConfigRegionAccess>,
    pci: &impl ConfigRegionAccess,
    devices: &[PciAddress],
) {
    for &device in devices {
        // SAFETY: supplied live aligned PCI config mapping.
        let id = unsafe { pci.read(device, 0) };
        if id != 0x1c5c1022 {
            continue;
        }
        let lc_cntl = smn.reg(SmnReg::<()>::new(pcie::port_raw(usize::from(device.function()), 0x280)));
        let value = lc_cntl.get();
        lc_cntl.set((value & 0x00ffffff) | (((value >> 24 & 0xe9) | 0x14) << 24));
    }
    smn.reg(pcie::cntl(0))
        .modify(pcie::Cntl::HWINIT_WR_LOCK::CLEAR);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;
    use crate::picasso::dxio::Descriptor;
    #[test]
    fn both_l1_modes_enable_gate_without_requiring_l1_substates() {
        for (aspm, gate) in [
            (Aspm::Disabled, false),
            (Aspm::L0s, false),
            (Aspm::L1, true),
            (Aspm::L0sL1, true),
        ] {
            let mut topology = Topology::default();
            let mut descriptor = Descriptor::pcie(0, 0, 1, 1);
            descriptor.aspm = aspm;
            descriptor.l1_1 = false;
            descriptor.l1_2 = false;
            topology.ports[0].descriptor = Some(descriptor);
            topology.ports[0].trained = true;
            let space = SmnSpace::default();
            space.set(pcie::lc_cntl6(0), 0x40);
            port_power_management(&space.smn(), &topology);
            assert_eq!(
                space.get(pcie::lc_cntl6(0)),
                0x40 | if gate { 1 << 21 } else { 0 }
            );
        }
    }
    #[test]
    fn clock_mapping_is_a_permutation_with_lower_port_winning_shared_pin() {
        let mut topology = Topology::default();
        let mut d = Descriptor::pcie(0, 0, 1, 1);
        d.l1_2 = true;
        d.clock_request = ClockRequest::Pin0;
        for i in [1, 4] {
            topology.ports[i].descriptor = Some(d);
            topology.ports[i].trained = true;
        }
        let map = clock_map(&topology);
        assert_eq!((map >> 4) & 15, 0);
        let mut pins = [0u8; 8];
        for (i, pin) in pins.iter_mut().enumerate() {
            *pin = (map >> (i * 4) & 15) as u8;
        }
        pins.sort();
        assert_eq!(pins, [0, 1, 2, 3, 4, 5, 6, 7]);
        topology.ports[1].trained = false;
        assert_eq!(clock_map(&topology) >> 16 & 15, 0);
    }
}
