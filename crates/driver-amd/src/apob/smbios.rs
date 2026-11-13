// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMBIOS inventory of an AMD system from its APOB: the DIMM positions
//! the ABL reports with the modules of the APOB's SPD copies, and the
//! processor packages, added to the shared SMBIOS builder. The platform
//! supplies the packages' CPUID, the DRAM ranges and the board identity.
extern crate alloc;

use super::{Apob, mem};
use alloc::{format, vec::Vec};
use fstart_acpi::smbios::{Builder, Package};
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
use fstart_core::memory_info::MemoryDevice;

/// SMBIOS processor family of Zen 1, 2 and 3.
pub const FAMILY_ZEN: u16 = 0x6b;

/// A DRAM range of the memory map, for type 19.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DramRange {
    pub base: u64,
    /// Exclusive.
    pub end: u64,
}

/// One homogeneous CPU package. APIC IDs include only enabled threads;
/// cache geometry is read on a real member of this package, not copied from BSP.
pub struct Processor<'a> {
    pub socket: u8,
    pub snapshot: ProcessorSnapshot,
    pub apic_ids: &'a [u32],
    pub enabled_cores: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Dimm {
    pub socket: u8,
    pub die: u8,
    pub device: MemoryDevice,
}

/// The DIMM positions of the APOB's SMBIOS seeds: die and channel from the
/// seed's channel (`channels_per_die` per die), the module from the SPD
/// copy of that die, empty or unknown positions as such. Nothing if a
/// position is invalid, duplicate, or on a die `die_exists` denies.
pub fn dimms(apob: &Apob, channels_per_die: u8, die_exists: impl Fn(u8, u8) -> bool) -> Vec<Dimm> {
    let Ok(seeds) = mem::smbios_info(apob) else {
        fstart_log::warn!("smbios: no memory seeds in the APOB");
        return Vec::new();
    };
    let mut dimms: Vec<Dimm> = Vec::new();
    for seed in seeds.positions() {
        let (die, channel) = (
            seed.channel / channels_per_die,
            seed.channel % channels_per_die,
        );
        if seed.dimm >= 2
            || !die_exists(seed.socket, die)
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
            let spd = mem::spd(apob, seed.socket, die).map_err(|_| "no SPD table");
            let module = spd.and_then(|table| {
                let (_, _, record) = table
                    .dimms()
                    .find(|(c, d, _)| *c == channel && *d == seed.dimm)
                    .ok_or("no SPD record")?;
                fstart_driver_spd::ddr4::inventory(
                    &record.jedec_spd(),
                    configured,
                    channel,
                    seed.dimm,
                )
                .map_err(spd_error)
            });
            module.unwrap_or_else(|why| {
                fstart_log::warn!(
                    "smbios: socket {} die {} channel {} DIMM {}: {}",
                    seed.socket,
                    die,
                    channel,
                    seed.dimm,
                    why
                );
                MemoryDevice::unknown(channel, seed.dimm)
            })
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

fn spd_error(error: fstart_driver_spd::ddr4::Error) -> &'static str {
    use fstart_driver_spd::ddr4::Error;
    match error {
        Error::TooShort => "SPD too short",
        Error::NotDdr4 => "not a DDR4 SPD",
        Error::Header => "SPD header",
        Error::Revision => "SPD revision",
        Error::Crc => "SPD CRC",
        Error::ModuleType => "hybrid module",
        Error::Organization => "SPD organization",
        Error::Timebase => "SPD timebase",
        Error::Clock => "SPD clock",
    }
}

/// The RAM of a memory map, for type 19.
pub fn dram_ranges<'a>(
    regions: impl IntoIterator<Item = &'a super::sysmap::Region>,
) -> Vec<DramRange> {
    regions
        .into_iter()
        .filter(|region| region.kind.is_ram())
        .map(|region| DramRange {
            base: region.base,
            end: region.end(),
        })
        .collect()
}

/// Add a package per processor: Zen, in the socket of SMBIOS upgrade code
/// `upgrade` (0x37 SP3, 0x06 none for the BGA packages).
pub fn add_packages<'a>(tables: &mut Builder<'a>, processors: &'a [Processor<'a>], upgrade: u8) {
    for processor in processors {
        tables.package(
            processor.socket,
            Package {
                cpu: &processor.snapshot,
                apic_ids: processor.apic_ids,
                cores: processor.snapshot.core_count.max(processor.enabled_cores),
                enabled_cores: processor.enabled_cores,
                threads: u16::try_from(processor.apic_ids.len()).unwrap_or(0),
                family: FAMILY_ZEN,
                upgrade,
                // No guessed BCLK, boost clock, or instantaneous frequency.
                external_clock_mhz: 0,
                max_speed_mhz: 0,
                current_speed_mhz: 0,
            },
        );
    }
}

/// Add the memory array of the DIMM positions `dimms`, each taking up to
/// `max_dimm_mib` (the supported maximum, not the installed capacity), and
/// its DRAM `regions` (the ABL's, before firmware allocations, not
/// `0..total_RAM`). `ecc` is APOB 8/8's active ECC mode; an SPD's ECC width
/// alone proves no active mode. Nothing without positions.
pub fn add_memory(
    tables: &mut Builder<'_>,
    dimms: &[Dimm],
    regions: &[DramRange],
    max_dimm_mib: u32,
    ecc: bool,
) {
    if dimms.is_empty() || max_dimm_mib == 0 {
        return;
    }
    tables.memory_array(
        u64::from(max_dimm_mib) * 1024 * dimms.len() as u64,
        if ecc { 6 } else { 3 },
    );
    for dimm in dimms {
        // These are explicit topology labels, not unverified silkscreen
        // names or DIMM-to-error/SEL mappings.
        let bank = format!(
            "Socket{} Die{} Channel{}",
            dimm.socket, dimm.die, dimm.device.channel
        );
        let locator = format!("{} DIMM{}", bank, dimm.device.slot);
        tables.dimm(&dimm.device, &locator, &bank);
    }
    for region in regions {
        // Channel/rank interleave width is not known from an address
        // range. SMBIOS uses zero for an unknown partition width.
        tables.mapped_range(region.base, region.end, 0);
    }
}

#[cfg(test)]
mod tests;
