//! CPPC as the SMU of Rome and Milan serves it (`smu::v11::cppc`): `_CPC`
//! for the processor devices and the PCCT with a subspace per channel.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::AmlError;
use fstart_acpi::processor::cpc_aml;
use fstart_driver_amd::smu::v11::{CppcInfo, cppc};

/// The PCC channel of a socket's SMU.
#[derive(Debug, Clone, Copy)]
pub struct Channel<'a> {
    pub info: CppcInfo,
    /// PCC region.
    pub base: u64,
    pub smu_bar: u64,
    pub lowest_mhz: u32,
    pub nominal_mhz: u32,
    /// APIC ID of each PCC slot.
    pub slots: &'a [u32],
}

/// `_CPC` of the thread `apic_id`: the registers of its slot in the
/// channel that serves it, the channel's index being its PCC subspace.
/// Nothing for a thread no channel serves; one in several is an error.
pub fn cpc(channels: &[Channel], apic_id: u32) -> Result<Vec<u8>, AmlError> {
    if channels.len() > 256 || channels.iter().any(|c| c.slots.len() > 256) {
        return Err(AmlError::InvalidOperand);
    }
    let mut serving = channels.iter().zip(0u8..).filter_map(|(channel, id)| {
        let slot = channel.slots.iter().position(|&a| a == apic_id)?;
        Some((channel, id, slot as u32))
    });
    let Some((c, id, slot)) = serving.next() else {
        return Ok(Vec::new());
    };
    if serving.next().is_some() {
        return Err(AmlError::InvalidOperand);
    }
    let entries = cppc::cpc_entries(&c.info, id, slot, c.lowest_mhz, c.nominal_mhz);
    let n = cppc::cpc_entry_count(c.info.revision);
    Ok(cpc_aml(c.info.revision, &entries[..n]))
}

/// The PCCT, `None` without a channel.
pub fn pcct(channels: &[Channel]) -> Option<Vec<u8>> {
    (!channels.is_empty()).then(|| {
        let subspaces: Vec<_> = channels
            .iter()
            .map(|c| cppc::pcct_subspace(c.base, c.smu_bar))
            .collect();
        fstart_acpi::pcct::build_pcct(&subspaces)
    })
}
