// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMBIOS tables of the flow ([`crate::smbios`]): the board's
//! identity, one processor package, the DIMM positions of the APOB's SMBIOS
//! seeds with their SPD copies, and the APOB's DRAM ranges.
extern crate alloc;

use super::{Board, Soc};
use crate::smbios::{Dimm, DramRange, Inputs, Processor, build_into};
use alloc::vec::Vec;
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
use fstart_core::memory_info::MemoryDevice;
use fstart_driver_amd::apob::{
    Apob, mem,
    sysmap::{self, MemoryMap},
};
use fstart_log::Hex;

/// The DIMM positions the ABL reports for `S`: die and channel from the
/// seed's channel (`channels_per_die` per die), the module from the APOB's
/// SPD copy of that die, empty or unknown positions as such.
fn dimms<S: Soc>(apob: &Apob) -> Vec<Dimm> {
    let Ok(seeds) = mem::smbios_info(apob) else {
        fstart_log::warn!("smbios: no memory seeds in the APOB");
        return Vec::new();
    };
    let mut dimms: Vec<Dimm> = Vec::new();
    for seed in seeds.positions() {
        let (die, channel) = (
            seed.channel / S::SMBIOS.channels_per_die,
            seed.channel % S::SMBIOS.channels_per_die,
        );
        if seed.dimm >= 2
            || dimms.iter().any(|d| {
                (d.socket, d.die, d.device.channel, d.device.slot)
                    == (seed.socket, die, channel, seed.dimm)
            })
        {
            fstart_log::warn!("smbios: invalid memory position; omitting the memory inventory");
            return Vec::new();
        }
        let device = if seed.present {
            let configured = mem::mem_config(apob, seed.socket, die)
                .ok()
                .and_then(|config| config.memclk().ok())
                .and_then(|mhz| mhz.checked_mul(2))
                .unwrap_or(0);
            mem::spd(apob, seed.socket, die)
                .ok()
                .and_then(|table| {
                    let (_, _, record) = table
                        .dimms()
                        .find(|(c, d, _)| *c == channel && *d == seed.dimm)?;
                    fstart_driver_spd::ddr4::inventory(&record.spd, configured, channel, seed.dimm)
                        .ok()
                })
                .unwrap_or_else(|| MemoryDevice::unknown(channel, seed.dimm))
        } else {
            MemoryDevice::empty(channel, seed.dimm)
        };
        dimms.push(Dimm {
            socket: seed.socket,
            die,
            device,
        });
    }
    dimms
}

/// Where legacy OSes look for the entry point.
const F_SEGMENT: u64 = 0xf_0000;

/// Build the tables into reserved firmware memory; returns their entry
/// point's address.
pub fn install<S: Soc, B: Board<Soc = S>>(
    apob: Option<&Apob>,
    map: &MemoryMap,
    apic_ids: &[u32],
) -> Option<u64> {
    #[repr(C, align(4096))]
    struct Tables([u8; fstart_acpi::smbios::BUFFER_SIZE]);
    static mut TABLES: Tables = Tables([0; fstart_acpi::smbios::BUFFER_SIZE]);

    let smt = (fstart_arch::x86::cpuid(0x8000_001e).1 >> 8) & 0xff != 0;
    let cores = apic_ids.iter().filter(|&&id| !smt || id & 1 == 0).count() as u16;
    let processors = [Processor {
        socket: 0,
        snapshot: ProcessorSnapshot::read(),
        apic_ids,
        enabled_cores: cores,
    }];
    let dimms = apob.map(dimms::<S>).unwrap_or_default();
    let regions: Vec<DramRange> = sysmap::memory_map(S::PSP, map.top, map.holes().iter().copied())
        .map(|regions| {
            regions
                .iter()
                .filter(|region| region.kind.is_ram())
                .map(|region| DramRange {
                    base: region.base,
                    end: region.end(),
                })
                .collect()
        })
        .unwrap_or_default();
    let ecc = apob
        .and_then(|apob| mem::smbios_info(apob).ok())
        .is_some_and(|seeds| seeds.ecc());
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    let address = storage.as_ptr() as u64;
    let used = build_into(
        storage,
        address,
        &Inputs {
            identity: B::smbios_identity(),
            processors: &processors,
            dimms: &dimms,
            regions: &regions,
            max_dimm_mib: S::SMBIOS.max_dimm_mib,
            socket_upgrade: S::SMBIOS.socket_upgrade,
            ecc,
            rom_size: S::LAYOUT.flash.size as u32,
            uefi: cfg!(fstart_payload = "crabefi"),
        },
    )
    .ok()?;
    // A copy of the entry point where legacy OSes (Linux without UEFI)
    // scan for it: the F segment, reserved DRAM (the fixed MTRRs route
    // C0000-FFFFF to DRAM).
    // SAFETY: the e820 reserves the F segment for the firmware.
    unsafe {
        core::ptr::copy_nonoverlapping(
            storage.as_ptr(),
            F_SEGMENT as usize as *mut u8,
            fstart_acpi::smbios::ENTRY_POINT_SIZE,
        )
    };
    fstart_log::info!(
        "smbios: {} DIMM positions, {} installed, {} bytes at {}",
        dimms.len() as u32,
        dimms.iter().filter(|d| d.device.is_populated()).count() as u32,
        used as u32,
        Hex(address)
    );
    Some(address)
}
