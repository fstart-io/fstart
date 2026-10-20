// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixed native Picasso cold-boot silicon flow. No FSP calls or HOBs.
use crate::memory::{self, Memory};
use fstart_arch::x86::cpu::msr::{MCG_CAP, MTRR_CAP, McgCap, MsrAccess, MtrrCap};
use fstart_driver_amd::picasso::{
    apob::MemoryMap,
    cache::{self, CacheControl, Plan},
    cpu::{self, Identity},
    fabric,
    nbio::{self, Capabilities},
    registers::{Df, Smn},
};
use tock_registers::interfaces::Readable;

pub struct Inputs<'a> {
    pub apob: &'a MemoryMap,
    pub cpuid_signature: u32,
    pub cpuid_extended_ebx: u32,
    pub physical_address_bits: u8,
    pub tseg_size: u64,
}
pub struct State {
    pub memory: Memory,
    pub caches: Plan,
    pub cpu: Identity,
    pub capabilities: Capabilities,
}
#[derive(Debug)]
pub enum Error {
    Cpu(cpu::Error),
    Cache(cache::Error),
    Memory(memory::Error),
    Fabric(fabric::Error),
    Nbio(nbio::Error),
}

/// Cold BSP only, with interrupts and caching disabled at entry. Transports
/// hold exclusive register access. The later native flow owns DXIO, device,
/// MP/SMM and payload handoff; returning this state is NOT a boot-success claim.
pub fn initialize(
    input: Inputs<'_>,
    msr: &(impl MsrAccess + CacheControl),
    smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
    df: &Df<impl fstart_pci::ConfigRegionAccess>,
    delay_us: &mut impl FnMut(u32),
) -> Result<State, Error> {
    // Validate CPU, memory reservations and the full MTRR plan before writes.
    let initial_id = Identity::from_cpuid(input.cpuid_signature, input.cpuid_extended_ebx, 0)
        .map_err(Error::Cpu)?;
    let (low, high) = cpu::memory_limits(msr).map_err(Error::Cpu)?;
    let memory =
        Memory::from_apob(input.apob, low, high, input.tseg_size).map_err(Error::Memory)?;
    let mtrrs = msr.reg(MTRR_CAP).read(MtrrCap::VARIABLE_COUNT) as usize;
    let caches = Plan::new(low, input.physical_address_bits, mtrrs).map_err(Error::Cache)?;
    let banks = msr.reg(MCG_CAP).read(McgCap::COUNT) as u8;
    if !(17..=32).contains(&banks) {
        return Err(Error::Cpu(cpu::Error::McaBanks(banks)));
    }
    cache::install(msr, &caches).map_err(Error::Cache)?;
    cpu::enable_ecam(msr);
    fabric::ensure_legacy_routes(df, low as u32).map_err(Error::Fabric)?;
    fabric::early_init(df);
    let capabilities = nbio::early_init(smn, initial_id.raven2_cpuid, initial_id.am4, delay_us)
        .map_err(Error::Nbio)?;
    let identity = Identity::from_cpuid(
        input.cpuid_signature,
        input.cpuid_extended_ebx,
        capabilities.silicon_id,
    )
    .map_err(Error::Cpu)?;
    nbio::psp_bar(smn).map_err(Error::Nbio)?;
    cpu::bsp_init(msr);
    cpu::mca_init(msr).map_err(Error::Cpu)?;
    // Default topology is retained here; the full flow applies the requested
    // board CPU topology before releasing any APs through the SMU.
    let (low, high) = cpu::memory_limits(msr).map_err(Error::Cpu)?;
    nbio::dram_routes(smn, low, high).map_err(Error::Nbio)?;
    Ok(State {
        memory,
        caches,
        cpu: identity,
        capabilities,
    })
}
