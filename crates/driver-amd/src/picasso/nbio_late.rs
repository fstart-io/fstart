// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native PCIe power/CLKREQ and late SMU notifications.
use super::{
    cpu::{Identity, Silicon},
    dxio::{Aspm, ClockRequest, Hotplug, Topology},
    power,
    registers::Smn,
    smu,
};
use fstart_pci::{ConfigRegionAccess, PciAddress};
fn port(i: usize, offset: u32) -> u32 {
    0x11140000 + i as u32 * 0x1000 + offset
}

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
pub fn port_power_management(smn: &mut impl Smn, topology: &Topology) {
    smn.update(
        0x11180460,
        (1 << 16) | (3 << 9) | (3 << 12),
        1 | (1 << 9) | 2 | 4 | (1 << 6) | (1 << 12) | (1 << 14) | (1 << 24) | (1 << 25),
    );
    smn.update(0x11180044, 15 << 20, (1 << 20) | (1 << 21));
    smn.update(0x1118018c, 0, 0x4060);
    for i in 0..7 {
        smn.update(port(i, 0x1a8), 0, 1);
    }
    for (i, p) in topology.ports.iter().enumerate().rev() {
        if p.descriptor
            .is_some_and(|d| matches!(d.aspm, Aspm::L1 | Aspm::L0sL1))
        {
            smn.update(port(i, 0x2ec), 0, 1 << 21);
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
            smn.update(port(i, 0x2ec), 0, 1 << 21);
            if d.l1_2 {
                for j in 0..8 {
                    smn.update(port(j, 0x304), 0, 1 << 5);
                }
            }
        } else if d.hotplug == Hotplug::Disabled {
            smn.update(0x11100378 + i as u32 * 0x1000, 15, 15);
            smn.update(port(i, 0x2ec), 0, 1 << 21);
        }
    }
    smn.write(0x111800f4, clock_map(topology));
    for (address, bit) in [
        (0x02d01eb0, 8),
        (0x02d01eb0, 24),
        (0x02d01eb4, 25),
        (0x02d01eb4, 24),
    ] {
        smn.update(address, 1 << bit, 0);
    }
}
/// # Safety
/// Supply only topology returned by successful native training and permanently
/// reserved DMA pages meeting power::install's contract. Hardware changes are
/// fail-stop, not rollbackable; never call on a partially failed training flow.
pub unsafe fn pre_enumeration(
    smn: &mut impl Smn,
    topology: &Topology,
    pages: &'static mut power::Pages,
    config: power::Config,
    delay_us: &mut impl FnMut(u32),
    notice: &mut impl FnMut(u32, u32),
) -> Result<(), power::Error> {
    port_power_management(smn, topology);
    smn.update(0x11a0b7c0, 3, 1);
    unsafe { power::install(smn, pages, config, delay_us, notice) }
}
/// Notify SMU only after all requested CPUs reached native initialization.
/// Caller must retain nonstandard-response diagnostics rather than claiming
/// their requested features were verified active.
pub fn ccx_done(
    smn: &mut impl Smn,
    graphics: bool,
    delay_us: &mut impl FnMut(u32),
    notice: &mut impl FnMut(u32, u32),
) -> Result<(), power::Error> {
    let high = smn.read(0x0005d5bc);
    let low = smn.read(0x0005d5b8);
    let chip_id = ((high << 5) | (low >> 27)) & 0xffff;
    if chip_id == 0x62e {
        power::command(smn, 0x35, [14, 0, 0, 0, 0, 0], delay_us, notice)?;
    }
    if chip_id == 0x633 {
        power::command(smn, 0x35, [8, 0, 0, 0, 0, 0], delay_us, notice)?;
    }
    let features = 0xf7fffffd & if graphics { u32::MAX } else { !(1 << 20) };
    power::command(smn, 5, [features, 0x8f7, 0, 0, 0, 0], delay_us, notice)?;
    power::command(smn, 0x0e, [0; 6], delay_us, notice)?;
    if !graphics {
        power::command(smn, 0x53, [0; 6], delay_us, notice)?;
    }
    for (message, first, second) in [(0x3a, 184, 0), (0x42, 0, 0), (0x32, 0, 0), (5, 0, 1 << 8)] {
        power::command(smn, message, [first, second, 0, 0, 0, 0], delay_us, notice)?;
    }
    Ok(())
}
/// Final step AFTER native graphics setup. The last RV2/PCO request is strict,
/// unlike the specifically documented late-policy nonstandard responses.
pub fn post_silicon(
    smn: &mut impl Smn,
    identity: Identity,
    delay_us: &mut impl FnMut(u32),
) -> Result<(), smu::Error> {
    smn.update(0x13b38404, 0, 5); // IOHC internal bridge 29
    smn.update(0x10135000, 1 << 28, 0);
    smn.update(0x03100000, 0, 1);
    smn.update(0x03100000, 0, 1 << 11);
    if matches!(identity.silicon, Silicon::Picasso | Silicon::Raven2) {
        smu::request(smn, 0x11, [0; 6], delay_us)?;
    }
    Ok(())
}
/// `devices` must contain enabled enumerated PCI functions. Retains the native
/// 1c5c quirk's function-number SMN index; do not substitute a devfn remap index.
pub fn after_enumeration(
    smn: &mut impl Smn,
    pci: &impl ConfigRegionAccess,
    devices: &[PciAddress],
) {
    for &device in devices {
        // SAFETY: supplied live aligned PCI config mapping.
        let id = unsafe { pci.read(device, 0) };
        if id != 0x1c5c1022 {
            continue;
        }
        let address = port(usize::from(device.function()), 0x280);
        let value = smn.read(address);
        smn.write(
            address,
            (value & 0x00ffffff) | (((value >> 24 & 0xe9) | 0x14) << 24),
        );
    }
    smn.update(0x11180040, 1, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picasso::dxio::Descriptor;
    extern crate std;
    #[derive(Default)]
    struct Registers(std::collections::BTreeMap<u32, u32>);
    impl Smn for Registers {
        fn read(&mut self, address: u32) -> u32 {
            self.0.get(&address).copied().unwrap_or(0)
        }
        fn write(&mut self, address: u32, value: u32) {
            self.0.insert(address, value);
        }
    }
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
            let mut registers = Registers::default();
            registers.0.insert(port(0, 0x2ec), 0x40);
            port_power_management(&mut registers, &topology);
            assert_eq!(
                registers.0[&port(0, 0x2ec)],
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
