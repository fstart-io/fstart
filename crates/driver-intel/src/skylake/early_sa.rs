// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded Skylake/Kaby Lake SA decode and cold legacy-memory preparation.
//!
//! Derived from coreboot common/block/systemagent/systemagent_early.c and
//! skylake/romstage/systemagent.c, with offsets/resources from their headers.
//! This is NOT systemagent_early_init: VT-d, sa_pre_mem_native, memory training,
//! final memory-map programming and security/graphics/CSE work remain external.
//! BAR address masks and high address/lock fields are not specified by those
//! sources. Only enable bit 0 is modeled; unexplained low bits reject instead
//! of being overwritten. The supported resources are fixed below 4 GiB.
//!
//! Native SHA-256 anchors (unmodified source checkout):
//! * common/block/systemagent/systemagent_early.c:
//!   2b87db45eabbe90214abc4c37605e36b2710a684604dacade58fa87774956ab1
//! * skylake/romstage/systemagent.c:
//!   2550e572bb3698c38e509177d2564a4ccc24a73c89038b3dd2156ffe4a2f74b0
//! * skylake/include/soc/iomap.h:
//!   98fd8727754bafcb16eb48cbc6a6c8881c4ab926ec4410fd28fd51f509dc6c66

use super::capabilities::CapidA;
use super::registers::{MCHBAR_BASE, MCHBAR_SIZE, MchRegisters, SaDecode};
use crate::sunrise_point::{pcr, pmc::MemoryBootMode};
use fstart_pci::ecam::{self, EcamDevice};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

pub const DMI_BASE: u32 = 0xfed1_8000;
pub const EP_BASE: u32 = 0xfed1_9000;
pub const EDRAM_BASE: u32 = 0xfed8_0000;
pub const GDXC_BASE: u32 = 0xfed8_4000;
pub const EDRAM_SIZE: u32 = 0x4000;
pub const GDXC_SIZE: u32 = 0x1000;

const PCIEXBAR: u16 = 0x60;
const PCI_BARS: [(u16, u32, u32); 3] = [
    (0x48, MCHBAR_BASE as u32, MCHBAR_SIZE as u32),
    (0x68, DMI_BASE, 0x1000),
    (0x40, EP_BASE, 0x1000),
];

register_bitfields![u32,
    PciExBar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LENGTH OFFSET(1) NUMBITS(3) []
    ],
    // memory_map_native.c states address fields start at bit 20;
    // skylake/systemagent.c:systemagent_native_security_lock sets TSEG bit 0.
    Tseg [
        LOCK OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(20) NUMBITS(12) []
    ]
];
register_bitfields![u8,
    Pam0 [
        LOCK OFFSET(0) NUMBITS(1) [],
        DRAM OFFSET(4) NUMBITS(2) []
    ],
    Pam [
        LOW_DRAM OFFSET(0) NUMBITS(2) [],
        HIGH_DRAM OFFSET(4) NUMBITS(2) []
    ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidGeometry,
    Overlap,
    WrongFunction,
    WrongHostBridge,
    UnmappedConfig,
    RetainedMemory,
    ConflictingDecode,
    UnexplainedBarBits,
    DecodeDisabled,
    Locked,
    Readback,
    Originators(pcr::Error),
}

/// Bus-zero ECAM geometry. Only the native early path's 64/128/256 buses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamConfig {
    pub base: u64,
    pub buses: u16,
}
impl EcamConfig {
    /// Validate the complete prefix resource geometry before any hardware I/O.
    pub fn encoded_pciexbar(self) -> Result<u32, Error> {
        let length = match self.buses {
            256 => 0,
            128 => 1,
            64 => 2,
            _ => return Err(Error::InvalidGeometry),
        };
        let size = u64::from(self.buses) << 20;
        let ecam_end = range_end(self.base, size)?;
        // Include the native fixed PCH preserved/PWRM/SPI and VT-d windows so
        // ECAM cannot consume those source-owned resources either.
        let ranges = [
            (MCHBAR_BASE as u64, MCHBAR_SIZE as u64),
            (u64::from(DMI_BASE), 0x1000),
            (u64::from(EP_BASE), 0x1000),
            (u64::from(EDRAM_BASE), u64::from(EDRAM_SIZE)),
            (u64::from(GDXC_BASE), u64::from(GDXC_SIZE)),
        ];
        let reservations = [
            (0xfc80_0000, 0x0200_0000),
            (0xfe00_0000, 0x10000),
            (0xfe01_0000, 0x1000),
            (0xfed9_0000, 0x4000),
        ];
        // Actual decodes cannot overlap one another. The enclosing PCH
        // preserved reservation intentionally contains several fixed decodes.
        for (i, &(base, bytes)) in ranges.iter().enumerate() {
            let end = range_end(base, bytes)?;
            for &(other, other_bytes) in &ranges[..i] {
                if base < other + other_bytes && other < end {
                    return Err(Error::Overlap);
                }
            }
        }
        for &(base, bytes) in ranges.iter().chain(&reservations) {
            // Reservations need not be naturally aligned to their extent:
            // native PCH preserved space starts at fc800000 and spans 32 MiB.
            let end = base
                .checked_add(bytes)
                .filter(|&end| end <= 1 << 32)
                .ok_or(Error::InvalidGeometry)?;
            if self.base < end && base < ecam_end {
                return Err(Error::Overlap);
            }
        }
        Ok(self.base as u32 | PciExBar::LENGTH.val(length).value | PciExBar::ENABLE::SET.value)
    }
}

fn range_end(base: u64, size: u64) -> Result<u64, Error> {
    if base == 0 || !size.is_power_of_two() || base & (size - 1) != 0 {
        return Err(Error::InvalidGeometry);
    }
    let end = base.checked_add(size).ok_or(Error::InvalidGeometry)?;
    if end > 1 << 32 {
        return Err(Error::InvalidGeometry);
    }
    Ok(end)
}

fn host_identity(id: u32, class_revision: u32, header: u8) -> Result<(), Error> {
    // Exact SKL/KBL northbridge IDs from source include/device/pci_ids.h.
    // No broad high-byte family match admitting Coffee Lake or unknown IDs.
    let device = (id >> 16) as u16;
    if id as u16 != 0x8086
        || !matches!(
            device,
            0x1904
                | 0x190c
                | 0x1924
                | 0x1900
                | 0x1910
                | 0x190f
                | 0x191f
                | 0x1918
                | 0x590f
                | 0x5904
                | 0x590c
                | 0x5910
                | 0x5914
                | 0x5918
                | 0x591f
        )
        || class_revision >> 8 != 0x060000
        || header & 0x7f != 0
    {
        return Err(Error::WrongHostBridge);
    }
    Ok(())
}

pub(crate) fn validate_host(sa: &EcamDevice) -> Result<(), Error> {
    if sa.address().segment() != 0 || sa.bus() != 0 || sa.dev() != 0 || sa.func() != 0 {
        return Err(Error::WrongFunction);
    }
    host_identity(
        sa.try_read32(0).ok_or(Error::UnmappedConfig)?,
        sa.try_read32(8).ok_or(Error::UnmappedConfig)?,
        sa.try_read8(0x0e).ok_or(Error::UnmappedConfig)?,
    )
}

fn cold(mode: MemoryBootMode) -> Result<(), Error> {
    if mode != MemoryBootMode::Cold {
        return Err(Error::RetainedMemory);
    }
    Ok(())
}

// Planning uses the source resource alignment, NOT an asserted hardware
// address-field mask. Only exact matching live mappings are supported. Low
// control/reserved state not explained by the native source is rejected.
fn plan_decode(low: u32, high: u32, base: u32, size: u32) -> Result<bool, Error> {
    range_end(u64::from(base), u64::from(size))?;
    if low & (size - 1) & !SaDecode::ENABLE::SET.value != 0 {
        return Err(Error::UnexplainedBarBits);
    }
    if low & SaDecode::ENABLE::SET.value != 0 {
        if low != base | SaDecode::ENABLE::SET.value || high != 0 {
            return Err(Error::ConflictingDecode);
        }
        Ok(false)
    } else {
        Ok(true)
    }
}

fn plan_pciexbar(config: EcamConfig, low: u32, high: u32) -> Result<bool, Error> {
    let wanted = config.encoded_pciexbar()?;
    if low & 0x03ff_fff0 != 0 {
        return Err(Error::UnexplainedBarBits);
    }
    if low & PciExBar::ENABLE::SET.value != 0 {
        if low != wanted || high != 0 {
            return Err(Error::ConflictingDecode);
        }
        Ok(false)
    } else {
        Ok(true)
    }
}

/// Bootstrap only PCIEXBAR; never clear TSEG or touch PAM before PMC capture.
///
/// # Safety
/// Requires privileged x86 firmware, exclusive CF8/CFC and global ECAM
/// ownership. The validated physical ECAM range must be mapped identically
/// before this call; it must not overlap any other platform-owned resources
/// beyond those checked here. No retained-memory state is modified. This is
/// hardware-only: valid CF8 port I/O must never execute in a host test.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub unsafe fn bootstrap_ecam(config: EcamConfig) -> Result<EcamDevice, Error> {
    unsafe { bind_ecam(config) }
}

unsafe fn bind_ecam(config: EcamConfig) -> Result<EcamDevice, Error> {
    let wanted = config.encoded_pciexbar()?;
    // SAFETY: firmware-only exclusive CF8/CFC ownership is the caller contract.
    let program = unsafe {
        host_identity(
            fstart_pci::cf8::read32(0, 0, 0, 0),
            fstart_pci::cf8::read32(0, 0, 0, 8),
            (fstart_pci::cf8::read32(0, 0, 0, 0x0c) >> 16) as u8,
        )?;
        plan_pciexbar(
            config,
            fstart_pci::cf8::read32(0, 0, 0, PCIEXBAR as u8),
            fstart_pci::cf8::read32(0, 0, 0, (PCIEXBAR + 4) as u8),
        )?
    };
    if program {
        unsafe {
            // The two source bootstrap writes, including stale-upper clearing.
            fstart_pci::cf8::write32(0, 0, 0, (PCIEXBAR + 4) as u8, 0);
            fstart_pci::cf8::write32(0, 0, 0, PCIEXBAR as u8, wanted);
        }
    }
    // Check via CF8 before enabling software ECAM access: a failed decoder
    // write must not lead to an MMIO access through a nonoperational aperture.
    unsafe {
        if fstart_pci::cf8::read32(0, 0, 0, PCIEXBAR as u8) != wanted
            || fstart_pci::cf8::read32(0, 0, 0, (PCIEXBAR + 4) as u8) != 0
        {
            return Err(Error::Readback);
        }
    }
    ecam::init(config.base as usize);
    let sa = EcamDevice::new(0, 0, 0);
    validate_host(&sa)?;
    require_ecam(&sa, config)?;
    Ok(sa)
}

pub(crate) fn require_ecam(sa: &EcamDevice, config: EcamConfig) -> Result<(), Error> {
    let wanted = config.encoded_pciexbar()?;
    let low = sa.try_read32(PCIEXBAR).ok_or(Error::UnmappedConfig)?;
    if low & 1 == 0 {
        return Err(Error::DecodeDisabled);
    }
    if low != wanted || sa.try_read32(PCIEXBAR + 4).ok_or(Error::UnmappedConfig)? != 0 {
        return Err(Error::ConflictingDecode);
    }
    Ok(())
}

fn legacy_unlocked(sa: &EcamDevice) -> Result<(), Error> {
    let pam = unsafe { sa.register::<u8, Pam0::Register>(0x80) };
    let tseg = unsafe { sa.register::<u32, Tseg::Register>(0xb8) };
    // Even matching locked state rejects: the later cold memory-map phase
    // needs TSEG writable. Do not claim an unlocked cold prefix on late entry.
    if pam.is_set(Pam0::LOCK) || tseg.is_set(Tseg::LOCK) {
        return Err(Error::Locked);
    }
    Ok(())
}

/// First cold phase: enable fixed MCH/DMI/EP PCI decodes, individually.
///
/// PMC reset/wake capture must precede this call. The caller owns a stable
/// ECAM mapping and host bridge. All PCI table entries and legacy locks are
/// checked before writes. Matching enabled entries are left alone, not a
/// reason to skip later entries. Failure/readback mismatch forbids progress;
/// no rollback or usable-memory claim is made.
pub fn enable_pci_decodes_cold(
    sa: &EcamDevice,
    config: EcamConfig,
    mode: MemoryBootMode,
) -> Result<(), Error> {
    cold(mode)?;
    config.encoded_pciexbar()?;
    validate_host(sa)?;
    require_ecam(sa, config)?;
    legacy_unlocked(sa)?;
    let mut program = [false; 3];
    for (i, &(offset, base, size)) in PCI_BARS.iter().enumerate() {
        program[i] = plan_decode(sa.read32(offset), sa.read32(offset + 4), base, size)?;
    }
    for (i, &(offset, base, _)) in PCI_BARS.iter().enumerate() {
        if program[i] {
            unsafe {
                sa.register::<u32, ()>(offset + 4).set(0);
                sa.register::<u32, SaDecode::Register>(offset)
                    .set(base | SaDecode::ENABLE::SET.value);
            }
        }
        if sa.read32(offset) != base | 1 || sa.read32(offset + 4) != 0 {
            return Err(Error::Readback);
        }
    }
    Ok(())
}

/// Second cold phase: fixed GDXC/EDRAM decodes, PAM DRAM routing and TSEG clear.
///
/// `mch` must reference the mapped MCHBAR enabled by the first phase (a virtual
/// alias is allowed). Its existing unsafe binding contract supplies MMIO
/// ownership; this routine does not create a second overlay or transport.
/// Every PCI decode must still match, and both auxiliary decodes and legacy
/// locks are preflighted before any write in this phase. A conflict here can
/// leave the first phase's PCI decodes enabled, but no auxiliary/PAM/TSEG write
/// occurs. These phases establish decodes only, not usable DRAM or SMM setup.
pub fn prepare_auxiliary_pam_tseg_cold(
    sa: &EcamDevice,
    mch: &MchRegisters,
    config: EcamConfig,
    mode: MemoryBootMode,
) -> Result<(), Error> {
    cold(mode)?;
    config.encoded_pciexbar()?;
    validate_host(sa)?;
    require_ecam(sa, config)?;
    for &(offset, base, size) in &PCI_BARS {
        if plan_decode(sa.read32(offset), sa.read32(offset + 4), base, size)? {
            return Err(Error::DecodeDisabled);
        }
    }
    legacy_unlocked(sa)?;
    let gdxc = plan_decode(
        mch.gdxc_bar.get(),
        mch.gdxc_bar_high.get(),
        GDXC_BASE,
        GDXC_SIZE,
    )?;
    let edram = plan_decode(
        mch.edram_bar.get(),
        mch.edram_bar_high.get(),
        EDRAM_BASE,
        EDRAM_SIZE,
    )?;
    // Native order is GDXC, then EDRAM. Explicit dword writes, never a u64
    // access whose width/order differs from the source.
    for (low, high, base, program) in [
        (&mch.gdxc_bar, &mch.gdxc_bar_high, GDXC_BASE, gdxc),
        (&mch.edram_bar, &mch.edram_bar_high, EDRAM_BASE, edram),
    ] {
        if program {
            high.set(0);
            low.set(base | SaDecode::ENABLE::SET.value);
        }
        if low.get() != base | 1 || high.get() != 0 {
            return Err(Error::Readback);
        }
    }
    let pam0 = unsafe { sa.register::<u8, Pam0::Register>(0x80) };
    let desired0 = Pam0::DRAM.val(3).modify(pam0.get());
    pam0.set(desired0);
    if pam0.get() != desired0 {
        return Err(Error::Readback);
    }
    for offset in 0x81..=0x86 {
        let pam = unsafe { sa.register::<u8, Pam::Register>(offset) };
        let desired = (Pam::LOW_DRAM.val(3) + Pam::HIGH_DRAM.val(3)).modify(pam.get());
        pam.set(desired);
        if pam.get() != desired {
            return Err(Error::Readback);
        }
    }
    let tseg = unsafe { sa.register::<u32, Tseg::Register>(0xb8) };
    let desired = Tseg::BASE.val(0).modify(tseg.get());
    tseg.set(desired);
    if tseg.get() != desired {
        return Err(Error::Readback);
    }
    Ok(())
}

/// Result of early decode setup only. Neither variant certifies translation,
/// interrupt remapping, DMA protection or initialized/usable DRAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtdDecodeStatus {
    FusedOff,
    Configured { graphics: bool },
}

pub const GFX_VTD_BASE: u32 = 0xfed9_0000;
pub const VTVC0_BASE: u32 = 0xfed9_1000;

/// Native romstage/systemagent.c:systemagent_vtd_init's conditional decodes.
///
/// `igd_enabled` is the actual board/device policy, not an invented probe.
/// MCHBAR must already be operational and mapped under its unsafe constructor
/// contract; the P2SB must be visible. This routine does not hide/lock P2SB or
/// enable DMA/interrupt remapping. Source-ordered full SA composition, engine
/// initialization/security and memory-map reservation remain separate work.
pub fn prepare_vtd_decodes_cold(
    sa: &EcamDevice,
    mch: &MchRegisters,
    p2sb: &EcamDevice,
    config: EcamConfig,
    mode: MemoryBootMode,
    igd_enabled: bool,
) -> Result<VtdDecodeStatus, Error> {
    cold(mode)?;
    config.encoded_pciexbar()?;
    validate_host(sa)?;
    require_ecam(sa, config)?;
    // CAPID0_A.VTD_DISABLE is documented in soc/systemagent.h bit23.
    // The fuse gate precedes all MCH/P2SB access, as in the native routine.
    if unsafe { sa.register::<u32, CapidA::Register>(0xe4) }.is_set(CapidA::VTD_DISABLED) {
        return Ok(VtdDecodeStatus::FusedOff);
    }
    for &(offset, base, size) in &PCI_BARS {
        if plan_decode(sa.read32(offset), sa.read32(offset + 4), base, size)? {
            return Err(Error::DecodeDisabled);
        }
    }
    // Preflight every relevant live decoder before programming originators.
    let gfx = plan_decode(
        mch.gfx_vtd_bar.get(),
        mch.gfx_vtd_bar_high.get(),
        GFX_VTD_BASE,
        0x1000,
    )?;
    let vtvc0 = plan_decode(
        mch.vtvc0_bar.get(),
        mch.vtvc0_bar_high.get(),
        VTVC0_BASE,
        0x1000,
    )?;
    pcr::configure_vtd_originators(p2sb).map_err(Error::Originators)?;
    for (low, high, base, program, selected) in [
        (
            &mch.gfx_vtd_bar,
            &mch.gfx_vtd_bar_high,
            GFX_VTD_BASE,
            gfx,
            igd_enabled,
        ),
        (&mch.vtvc0_bar, &mch.vtvc0_bar_high, VTVC0_BASE, vtvc0, true),
    ] {
        if !selected {
            continue;
        }
        if program {
            high.set(0);
            low.set(base | SaDecode::ENABLE::SET.value);
        }
        if low.get() != base | 1 || high.get() != 0 {
            return Err(Error::Readback);
        }
    }
    Ok(VtdDecodeStatus::Configured {
        graphics: igd_enabled,
    })
}

#[cfg(test)]
#[path = "early_sa_tests.rs"]
mod tests;
