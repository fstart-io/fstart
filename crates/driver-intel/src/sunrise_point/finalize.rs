// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native finalize.c / lockdown.c and PCH-H P2SB endpoint masks. This is the
//! last sideband owner, AFTER actual device/SMM/CSE/SPI initialization.
use super::{
    hsio, pcr,
    pmc::{self, FinalLock, FunctionDisable, GenPmconA, PmcRegisters, PowerConfig},
};
use fstart_pci::EcamDevice;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
register_bitfields![u32,
    Xhci1 [
        STATUS_W1C OFFSET(23) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    Xhci2 [ LOCK OFFSET(31) NUMBITS(1) [] ],
    P2sbControl [ SBI_LOCK OFFSET(31) NUMBITS(1) [] ]
];
register_bitfields![u8,
    EndpointControl [ LOCK OFFSET(1) NUMBITS(1) [] ],
    Visibility [ HIDE OFFSET(0) NUMBITS(1) [] ]
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    Decode,
    Readback,
    Locked,
}
/// # Safety
/// Own actual UC PCH-H PMC/P2SB/xHCI PCI/PWRM and serialized sideband engines.
/// Device recipes, actual SMM install, SPI locks and CSE EOP/OS visibility have
/// completed. NO later PCR/SBI client may need the masked PSF/root-port/USB
/// endpoints. These irreversible locks are fatal on partial failure.
pub(crate) unsafe fn lock_native(
    pmc: &EcamDevice,
    p2sb: &EcamDevice,
    registers: &PmcRegisters,
    xhci_enabled: bool,
) -> Result<(), Error> {
    let lpc = EcamDevice::new(0, 0x1f, 0);
    let kind = hsio::Silicon::read(&lpc).map_err(|_| Error::Device)?.kind();
    if pmc.bus() != 0
        || pmc.dev() != 0x1f
        || pmc.func() != 2
        || pmc.vendor_id() != 0x8086
        || p2sb.bus() != 0
        || p2sb.dev() != 0x1f
        || p2sb.func() != 1
        || p2sb.vendor_id() != 0x8086
    {
        return Err(Error::Device);
    }
    if pmc.read32(0x48) != pmc::PWRM_BASE as u32
        || pmc.read32(0x44) & 0x100 == 0
        || p2sb.memory_bar(0x10) != Some(u64::from(pcr::DEFAULT_BASE))
    {
        return Err(Error::Decode);
    }
    let endpoint_lock = unsafe { p2sb.register::<u8, EndpointControl::Register>(0xe2) };
    if endpoint_lock.is_set(EndpointControl::LOCK) {
        return Err(Error::Locked);
    }
    if xhci_enabled {
        let xhci = EcamDevice::new(0, 0x14, 0);
        if xhci.vendor_id() != 0x8086 || xhci.read16(0x0a) != 0x0c03 {
            return Err(Error::Device);
        }
        let control2 = unsafe { xhci.register::<u32, Xhci2::Register>(0x44) };
        let control1 = unsafe { xhci.register::<u32, Xhci1::Register>(0x40) };
        control2.modify(Xhci2::LOCK::SET);
        control1.modify(Xhci1::STATUS_W1C::CLEAR + Xhci1::LOCK::SET);
        if !control2.is_set(Xhci2::LOCK) || !control1.is_set(Xhci1::LOCK) {
            return Err(Error::Readback);
        }
    }
    registers.pmsync.modify(FinalLock::LOCK::SET);
    registers
        .function_disable
        .modify(FunctionDisable::LOCK::SET);
    registers.power_config.modify(PowerConfig::PMC_LOCK::SET);
    if !registers.pmsync.is_set(FinalLock::LOCK)
        || !registers.function_disable.is_set(FunctionDisable::LOCK)
        || !registers.power_config.is_set(PowerConfig::PMC_LOCK)
    {
        return Err(Error::Readback);
    }
    let sbi = unsafe { p2sb.register::<u32, P2sbControl::Register>(0xe0) };
    sbi.modify(P2sbControl::SBI_LOCK::SET);
    if !sbi.is_set(P2sbControl::SBI_LOCK) {
        return Err(Error::Readback);
    }
    // Native p2sb_soc_get_sb_mask: LP adds MIPI/CAM bit 16 on EPMASK5.
    let epmask5 = 0x3c02_0402u32
        | if kind == super::dmi::Kind::Lp {
            1 << 16
        } else {
            0
        };
    for (index, mask) in [(5, epmask5), (7, 0x60)] {
        let endpoint = unsafe { p2sb.register::<u32, ()>(0xb0 + 4 * index) };
        endpoint.set(endpoint.get() | mask);
        if endpoint.get() & mask != mask {
            return Err(Error::Readback);
        }
    }
    endpoint_lock.modify(EndpointControl::LOCK::SET);
    if !endpoint_lock.is_set(EndpointControl::LOCK) {
        return Err(Error::Readback);
    }
    unsafe { p2sb.register::<u8, Visibility::Register>(0xe1) }.modify(Visibility::HIDE::SET);
    if p2sb.vendor_id() != 0xffff {
        return Err(Error::Readback);
    }
    // Native last PMC status RMW excludes MS4V; do not echo it as RW1C.
    unsafe { pmc.register::<u32, GenPmconA::Register>(0xa0) }.modify(GenPmconA::MS4V::CLEAR);
    Ok(())
}
