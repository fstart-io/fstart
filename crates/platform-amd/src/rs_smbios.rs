// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native SMBIOS of a single-socket Rs SoC: identity, processor and DDR5
//! inventory, using the common SMBIOS writer.
//! DDR5 inventory uses independently checked Rs records, never SP3 DDR4
//! layouts. Unknown asset identity/ECC mode is not manufactured.
use fstart_acpi::smbios::{self, Package, SmbiosIdentity};
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Error {
    Inventory,
    Storage,
    Memory,
    LegacyDecode,
    Readback,
}
#[derive(Clone, Copy)]
pub struct Memory<'a> {
    pub dimms: &'a [fstart_driver_amd::apob::smbios::Dimm],
    pub regions: &'a [fstart_driver_amd::apob::smbios::DramRange],
    pub max_dimm_mib: u32,
    /// The DMI header proves capability, not an active ECC algorithm.
    pub ecc: u8,
}
pub struct Inputs<'a> {
    pub identity: SmbiosIdentity<'a>,
    /// Processor socket designation ("SP5 Socket0", "SP6 Socket0").
    pub socket: &'a str,
    pub cpu: &'a ProcessorSnapshot,
    pub ids: &'a [u32],
    pub enabled_cores: u16,
    pub rom_size: u32,
    pub memory: Option<Memory<'a>>,
}
/// One SP5/SP6 socket, SMT2.
const MAX_CPUS: usize = 384;
pub fn build_into(storage: &mut [u8], address: u64, input: &Inputs<'_>) -> Result<usize, Error> {
    if input.cpu.vendor != *b"AuthenticAMD"
        || input.enabled_cores == 0
        || input.cpu.core_count < input.enabled_cores
        || input.ids.len() < usize::from(input.enabled_cores)
        || input.ids.len() > 2 * usize::from(input.enabled_cores)
        || input.ids.len() > MAX_CPUS
        || input
            .ids
            .iter()
            .enumerate()
            .any(|(i, id)| input.ids[..i].contains(id))
    {
        return Err(Error::Inventory);
    }
    if let Some(memory) = input.memory {
        let mut capacity = 0u64;
        if memory.dimms.is_empty()
            || memory.dimms.len() > 24
            || memory.max_dimm_mib == 0
            || !matches!(memory.ecc, 2 | 3 | 5 | 6)
        {
            return Err(Error::Inventory);
        }
        for (i, dimm) in memory.dimms.iter().enumerate() {
            let d = &dimm.device;
            if dimm.socket != 0
                || dimm.die != 0
                || d.channel >= 12
                || d.slot >= 2
                || d.size_mib.get() > memory.max_dimm_mib
                || (d.is_populated() && d.memory_type != fstart_core::memory_info::MEMORY_TYPE_DDR5)
                || memory.dimms[..i]
                    .iter()
                    .any(|p| (p.device.channel, p.device.slot) == (d.channel, d.slot))
            {
                return Err(Error::Inventory);
            }
            capacity += u64::from(d.size_mib.get()) << 20;
        }
        let mut mapped = 0u64;
        let mut end = 0;
        for region in memory.regions {
            if region.end <= region.base || region.base < end {
                return Err(Error::Inventory);
            }
            mapped = mapped
                .checked_add(region.end - region.base)
                .ok_or(Error::Inventory)?;
            end = region.end;
        }
        if mapped == 0 || mapped > capacity {
            return Err(Error::Inventory);
        }
    }
    smbios::assemble_into(storage, address, |writer| {
        writer.add_identity(&input.identity, input.rom_size, false);
        writer.add_package(&Package {
            socket: input.socket,
            cpu: input.cpu,
            apic_ids: input.ids,
            cores: input.cpu.core_count,
            enabled_cores: input.enabled_cores,
            threads: input.ids.len() as u16,
            family: 0x6b,
            upgrade: 2, // AMD Zen; socket encoding unknown in the 3.3 writer.
            external_clock_mhz: 0,
            max_speed_mhz: 0,
            current_speed_mhz: 0,
        });
        if let Some(memory) = input.memory {
            fstart_driver_amd::apob::smbios::add_memory_inventory(
                writer,
                memory.dimms,
                memory.regions,
                memory.max_dimm_mib,
                memory.ecc,
            );
        }
        writer.add_system_boot_info();
        writer.add_end_of_table();
    })
    .map_err(|_| Error::Storage)
}
/// Publish only after all APs park and no callback can touch shared MSRs.
/// Tables are allocated in the reserved firmware `heap`.
/// # Safety
/// Checked Rs CPU, IF=0, SMI disabled, the inherited/common MP cache plan
/// remains identical on every CPU. RAM proof is the checked PSP map, not OS E820.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub unsafe fn install(
    input: &Inputs<'_>,
    regions: &[fstart_driver_amd::apob::sysmap::Region],
    heap: core::ops::Range<u64>,
) -> Result<(), Error> {
    use fstart_arch::x86::{
        cpu::{
            amd::msr::{SYSCFG, Syscfg},
            msr::{MTRR_DEF_TYPE, MsrAccess, MtrrDefType, NativeMsr},
        },
        mtrr::IA32_MTRR_FIX4K_C0000,
    };
    use tock_registers::interfaces::Readable;
    if !super::rs_resources::ram_covers(
        regions,
        smbios::LEGACY_ENTRY_POINT..smbios::LEGACY_ENTRY_POINT + 0x1000,
    ) {
        return Err(Error::Memory);
    }
    let cpu = unsafe { NativeMsr::new() };
    if !cpu.reg(SYSCFG).is_set(Syscfg::MTRR_FIX_DRAM_EN)
        || !cpu.reg(MTRR_DEF_TYPE).is_set(MtrrDefType::FIXED_ENABLE)
        || !cpu.reg(MTRR_DEF_TYPE).is_set(MtrrDefType::ENABLE)
    {
        return Err(Error::LegacyDecode);
    }
    // F0000 is the first 4-KiB slot of the seventh 32-KiB C0000-bank MSR.
    // Reuse the existing gated AMD attribute view; do not install new MTRRs.
    let fixed = fstart_driver_amd::ccx::cache::fixed_dram_view(&cpu, |cpu| {
        cpu.read(IA32_MTRR_FIX4K_C0000 + 6)
    })
    .map_err(|_| Error::LegacyDecode)?;
    if fixed & 0xff != 0x1e {
        return Err(Error::LegacyDecode);
    }
    let layout = alloc::alloc::Layout::from_size_align(smbios::BUFFER_SIZE, 16)
        .map_err(|_| Error::Storage)?;
    let pointer = core::ptr::NonNull::new(unsafe { alloc::alloc::alloc_zeroed(layout) })
        .ok_or(Error::Storage)?;
    let address = pointer.as_ptr() as u64;
    if address < heap.start
        || address
            .checked_add(smbios::BUFFER_SIZE as u64)
            .is_none_or(|end| end > heap.end)
    {
        return Err(Error::Memory);
    }
    let storage = unsafe { core::slice::from_raw_parts_mut(pointer.as_ptr(), smbios::BUFFER_SIZE) };
    let used = build_into(storage, address, input)?;
    if storage[..smbios::ENTRY_POINT_SIZE]
        .iter()
        .fold(0u8, |sum, b| sum.wrapping_add(*b))
        != 0
    {
        return Err(Error::Storage);
    }
    unsafe { smbios::publish_legacy_entry_point(storage) };
    let entry = unsafe {
        core::slice::from_raw_parts(
            smbios::LEGACY_ENTRY_POINT as *const u8,
            smbios::ENTRY_POINT_SIZE,
        )
    };
    if entry != &storage[..smbios::ENTRY_POINT_SIZE] {
        return Err(Error::Readback);
    }
    fstart_log::info!(
        "rs: native SMBIOS entry={:#x} tables={:#x} bytes={} cores={} enabled={} threads={}; DIMM-positions={}",
        smbios::LEGACY_ENTRY_POINT,
        address,
        used,
        input.cpu.core_count,
        input.enabled_cores,
        input.ids.len(),
        input.memory.map_or(0, |m| m.dimms.len())
    );
    Ok(())
}
/// The board's SMBIOS identity and limits.
pub struct Board<'a> {
    pub manufacturer: &'a str,
    pub product: &'a str,
    pub socket: &'a str,
    pub rom_size: u32,
    /// Reporting capacity ceiling per DIMM, not population or a DIMM QVL.
    pub max_dimm_mib: u32,
    /// The UMC channels the board wires (bit per channel). The APOB describes
    /// every IOD channel, including those a smaller SoC (Siena) fuses off.
    pub channels: u16,
}

/// Inventory the checked APOB DDR5 seeds and the CPU set, then publish.
/// # Safety
/// As [`install`]: APs parked, IF/SMI off; tables go in the reserved heap.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub unsafe fn publish(
    board: &Board<'_>,
    apob: &fstart_driver_amd::apob::Apob<'_, fstart_driver_amd::apob::rs::Rs>,
    topology: &fstart_driver_amd::apob::ccx::CoreMap<12>,
    regions: &[fstart_driver_amd::apob::sysmap::Region],
    ids: &[u32],
    heap: core::ops::Range<u64>,
) -> Result<(), Error> {
    let mut dimms = fstart_driver_amd::apob::rs::memory::inventory(apob).map_err(|_| {
        fstart_log::error!("rs: DDR5 seed/SPD inventory rejected");
        Error::Inventory
    })?;
    if dimms
        .iter()
        .any(|d| d.device.is_populated() && board.channels & (1 << d.device.channel) == 0)
    {
        fstart_log::error!("rs: a DIMM is populated on a channel the board does not wire");
        return Err(Error::Inventory);
    }
    dimms.retain(|d| board.channels & (1 << d.device.channel) != 0);
    let dram = fstart_driver_amd::apob::smbios::dram_ranges(regions);
    for dimm in dimms.iter().filter(|d| d.device.is_populated()) {
        let d = &dimm.device;
        fstart_log::info!(
            "rs: DDR5 channel={} slot={} MiB={} MT/s={} configured={} mV={} data-bits={} total-bits={}",
            d.channel,
            d.slot,
            d.size_mib.get(),
            d.max_mts.get(),
            d.configured_mts.get(),
            d.voltage_mv.get(),
            d.data_width.get(),
            d.total_width.get()
        );
    }
    let processor = ProcessorSnapshot::read();
    let enabled_cores = topology
        .ccds
        .iter()
        .flat_map(|ccd| ccd.ccxs.iter())
        .flat_map(|ccx| ccx.cores.iter())
        .filter(|core| core.threads.contains(&true))
        .count() as u16;
    fstart_log::info!(
        "rs: SMBIOS CPU preflight CPUID-cores={} enabled-cores={} threads={}",
        processor.core_count,
        enabled_cores,
        ids.len()
    );
    // SAFETY: caller's contract.
    unsafe {
        install(
            &Inputs {
                identity: SmbiosIdentity::new(board.manufacturer, board.product),
                socket: board.socket,
                cpu: &processor,
                ids,
                enabled_cores,
                rom_size: board.rom_size,
                memory: Some(Memory {
                    dimms: &dimms,
                    regions: &dram,
                    max_dimm_mib: board.max_dimm_mib,
                    // Capability/width do not identify active correction policy.
                    ecc: 2,
                }),
            },
            regions,
            heap,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_inventory_is_checked_and_does_not_fabricate_ddr5_dimms() {
        let cpu = ProcessorSnapshot {
            vendor: *b"AuthenticAMD",
            brand: [0; 48],
            signature: 0xa10f10,
            standard_features: 0,
            standard_features_ecx: 0,
            extended_features: 1 << 29,
            extended_features_ecx: 0,
            core_count: 80,
            caches: [Default::default(); 4],
            cache_count: 0,
        };
        let ids: alloc::vec::Vec<_> = (0..160).collect();
        let input = Inputs {
            identity: SmbiosIdentity::new("AMD", "Onyx"),
            socket: "SP5 Socket0",
            cpu: &cpu,
            ids: &ids,
            enabled_cores: 80,
            rom_size: 0x2000000,
            memory: None,
        };
        let mut storage = alloc::vec![0;8192];
        let used = build_into(&mut storage, 0x8200000, &input).unwrap();
        assert_eq!(&storage[..5], b"_SM3_");
        assert_eq!(storage[..24].iter().fold(0u8, |s, b| s.wrapping_add(*b)), 0);
        let mut types = alloc::vec::Vec::new();
        let mut offset = smbios::ENTRY_POINT_SIZE;
        while offset < used {
            let kind = storage[offset];
            types.push(kind);
            if kind == 4 {
                assert_eq!(storage[offset + 0x23], 80);
                assert_eq!(storage[offset + 0x24], 80);
                assert_eq!(storage[offset + 0x25], 160);
            }
            offset += usize::from(storage[offset + 1]);
            while storage[offset..offset + 2] != [0, 0] {
                offset += 1;
            }
            offset += 2;
        }
        assert_eq!(types, alloc::vec![0, 1, 3, 2, 4, 32, 127]);
        use fstart_core::memory_info::{MEMORY_TYPE_DDR5, MemoryDevice};
        use fstart_driver_amd::apob::smbios::{Dimm, DramRange};
        let dimms: alloc::vec::Vec<_> = (0..12)
            .map(|channel| Dimm {
                socket: 0,
                die: 0,
                device: if channel == 3 {
                    MemoryDevice {
                        size_mib: 32768.into(),
                        memory_type: MEMORY_TYPE_DDR5,
                        data_width: 64.into(),
                        total_width: 80.into(),
                        ..MemoryDevice::empty(channel, 0)
                    }
                } else {
                    MemoryDevice::empty(channel, 0)
                },
            })
            .collect();
        let ranges = [
            DramRange {
                base: 0,
                end: 0xb0000000,
            },
            DramRange {
                base: 0x100000000,
                end: 0x850000000,
            },
        ];
        let memory = Memory {
            dimms: &dimms,
            regions: &ranges,
            max_dimm_mib: 256 * 1024,
            ecc: 2,
        };
        let full = Inputs {
            memory: Some(memory),
            ..input
        };
        let used = build_into(&mut storage, 0x8200000, &full).unwrap();
        let mut offset = 24;
        let mut array = 0;
        let mut devices = 0;
        let mut installed = 0;
        let mut maps = 0;
        while offset < used {
            let r = &storage[offset..];
            match r[0] {
                16 => {
                    array = u16::from_le_bytes([r[2], r[3]]);
                    assert_eq!(r[6], 2);
                    assert_eq!(u16::from_le_bytes([r[13], r[14]]), 12);
                }
                17 => {
                    devices += 1;
                    assert_eq!(u16::from_le_bytes([r[4], r[5]]), array);
                    if r[12..14] != [0, 0] {
                        installed += 1;
                        assert_eq!(r[18], 0x22);
                        assert_eq!(u32::from_le_bytes(r[28..32].try_into().unwrap()), 32768);
                    }
                }
                19 => {
                    maps += 1;
                    assert_eq!(r[14], 1);
                    assert_eq!(u16::from_le_bytes([r[12], r[13]]), array);
                }
                _ => {}
            }
            offset += r[1] as usize;
            while storage[offset..offset + 2] != [0, 0] {
                offset += 1;
            }
            offset += 2;
        }
        assert_eq!((devices, installed, maps), (12, 1, 2));
        let bad = Inputs {
            memory: Some(Memory {
                max_dimm_mib: 1,
                ..memory
            }),
            ..input
        };
        assert_eq!(
            build_into(&mut storage, 0x8200000, &bad),
            Err(Error::Inventory)
        );
        let duplicate = Inputs {
            ids: &[0, 0],
            enabled_cores: 1,
            ..input
        };
        assert_eq!(
            build_into(&mut storage, 0x8200000, &duplicate),
            Err(Error::Inventory)
        );
    }
}
