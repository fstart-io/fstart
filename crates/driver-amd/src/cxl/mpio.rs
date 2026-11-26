// SPDX-License-Identifier: MIT OR Apache-2.0
//! The CXL messages of the MPIO firmware, identical on Turin and Venice,
//! sent on an IOD's WH MPIO mailbox ([`crate::dxio::wh::Proxy`]):
//!
//! | ID | When | Arguments |
//! |---|---|---|
//! | `0x2C` | after training (PEI) | 0, list (32-bit), count |
//! | `0x0D` | after enumeration (DXE) | list\[63:32\], list\[31:0\], count |
//! | `0x30` | per CXL host bridge | trap MMIO lo/hi, DRAM shadow lo/hi |
//! | `0x30` | per IOD, last | arg4 = `0x8000_0000` (finalize) |
//!
//! A list is up to 16 [`DeviceInfo`] dwords of one IOD. Witnesses:
//! SilVenicePei `CxlDevListGenerateWh` `0xF6D16C..0xF6D18E`, AmdNbioDxe
//! `0x38D26`/`0x38548..0x38595`, CxlV2Manager `CxlSendMpioTrapInfo`
//! `0x20840..0x208C0` and `CxlSendMpioFinalization` `0x20900..0x2092A`
//! (instruction-checked, reports/research/CXL.md §2.3, §2.8).

use tock_registers::{LocalRegisterCopy, register_bitfields};

use super::{DeviceType, Version};
use crate::dxio::rs::mailbox::{ARGUMENT_COUNT, Error};
use crate::dxio::rs::session::CacheSync;
use crate::dxio::wh::Proxy;
use crate::smn::SmnAccess;

/// `BIOS_MPIO_MSG_CNLI_INITIALIZE`: the CXL devices after training.
pub const MSG_DEVICES: u16 = 0x2c;
/// `MPIO_MSG_CXL_INITIALIZE`: the CXL devices after enumeration.
pub const MSG_DEVICES_ENUMERATED: u16 = 0x0d;
/// The host bridge component register trap, and its finalization.
pub const MSG_HOST_BRIDGE: u16 = 0x30;
/// Argument 4 of the finalizing [`MSG_HOST_BRIDGE`].
pub const FINALIZE: u32 = 0x8000_0000;
/// Most devices in one IOD's list.
pub const MAX_DEVICES: usize = 16;

register_bitfields![u32,
    /// One CXL device of an MPIO list (`CXL_DEVICE_INFO`).
    pub DeviceInfo [
        /// The device's PCI function, device and bus; zero after training.
        FUNCTION OFFSET(0) NUMBITS(3) [],
        DEVICE OFFSET(3) NUMBITS(5) [],
        BUS OFFSET(8) NUMBITS(8) [],
        /// WH: the PCIe core (wrapper); Turin: the root bridge.
        IOHC OFFSET(16) NUMBITS(4) [],
        PORT OFFSET(20) NUMBITS(4) [],
        TYPE OFFSET(24) NUMBITS(4) [],
        VERSION OFFSET(28) NUMBITS(4) []
    ]
];

/// One CXL port's entry of an MPIO list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Device {
    /// `(bus, device, function)` of the device after enumeration.
    pub bdf: Option<(u8, u8, u8)>,
    /// The IOHC field's value (WH: the PCIe core).
    pub iohc: u8,
    pub port: u8,
    pub device_type: DeviceType,
    pub version: Version,
}

impl Device {
    pub fn info(&self) -> u32 {
        let (bus, device, function) = self.bdf.unwrap_or_default();
        let mut info = LocalRegisterCopy::<u32, DeviceInfo::Register>::new(0);
        info.write(
            DeviceInfo::FUNCTION.val(function.into())
                + DeviceInfo::DEVICE.val(device.into())
                + DeviceInfo::BUS.val(bus.into())
                + DeviceInfo::IOHC.val(self.iohc.into())
                + DeviceInfo::PORT.val(self.port.into())
                + DeviceInfo::TYPE.val(self.device_type as u32)
                + DeviceInfo::VERSION.val(self.version as u32),
        );
        info.get()
    }
}

/// A DMA buffer of one device list, as MPIO reads it.
#[repr(C, align(64))]
pub struct List(pub [u32; MAX_DEVICES]);

/// Write `devices` into `list` and make it visible to MPIO: the count.
fn fill(list: &mut List, devices: &[Device], sync: CacheSync) -> Result<u32, Error> {
    if devices.len() > MAX_DEVICES {
        return Err(Error::InvalidView);
    }
    for (slot, word) in list.0.iter_mut().zip(
        devices
            .iter()
            .map(Device::info)
            .chain(core::iter::repeat(0)),
    ) {
        // SAFETY: a valid, aligned dword of the buffer; volatile because
        // the firmware reads it behind the compiler's back.
        unsafe { fstart_core::mmio::write32(slot, word) };
    }
    // SAFETY: the buffer is `size_of::<List>()` bytes of memory we own.
    unsafe { sync((list as *const List).cast(), size_of::<List>()) };
    Ok(devices.len() as u32)
}

/// Tell MPIO the CXL devices of its IOD after training (`0x2C`).
///
/// # Safety
/// `mpio` is the IOD's MPIO, idle; `list` is identity-mapped DRAM below
/// 4 GiB that stays reserved until reset; `sync` writes it back to memory.
pub unsafe fn report_devices<A: SmnAccess>(
    mpio: &Proxy<'_, A>,
    list: &mut List,
    devices: &[Device],
    sync: CacheSync,
) -> Result<(), Error> {
    let count = fill(list, devices, sync)?;
    let address = u32::try_from(list as *const List as usize).map_err(|_| Error::InvalidView)?;
    let mut args = [0; ARGUMENT_COUNT];
    args[1] = address;
    args[2] = count;
    // SAFETY: the caller's contract; the list stays reserved.
    unsafe { mpio.request(MSG_DEVICES, args) }.map(drop)
}

/// Tell MPIO the CXL devices of its IOD with their enumerated BDF (`0x0D`).
///
/// # Safety
/// As [`report_devices`]; the list may be anywhere in identity-mapped DRAM.
pub unsafe fn report_enumerated<A: SmnAccess>(
    mpio: &Proxy<'_, A>,
    list: &mut List,
    devices: &[Device],
    sync: CacheSync,
) -> Result<(), Error> {
    let count = fill(list, devices, sync)?;
    let address = list as *const List as usize as u64;
    let mut args = [0; ARGUMENT_COUNT];
    args[0] = (address >> 32) as u32;
    args[1] = address as u32;
    args[2] = count;
    // SAFETY: the caller's contract; the list stays reserved.
    unsafe { mpio.request(MSG_DEVICES_ENUMERATED, args) }.map(drop)
}

/// The arguments of the host bridge trap message: MPIO serves accesses to
/// the MMIO window `trap` from the DRAM `shadow`.
pub const fn trap_arguments(trap: u64, shadow: u64) -> [u32; ARGUMENT_COUNT] {
    [
        trap as u32,
        (trap >> 32) as u32,
        shadow as u32,
        (shadow >> 32) as u32,
        0,
        0,
    ]
}

/// Hand one host bridge's component registers to MPIO (`0x30`).
///
/// # Safety
/// As [`report_devices`]; `shadow` is the host bridge's 64 KiB component
/// register copy ([`super::host::Block`]), reserved until reset, and
/// `trap` a window the fabric routes to this IOD that nothing else decodes.
pub unsafe fn trap_host_bridge<A: SmnAccess>(
    mpio: &Proxy<'_, A>,
    trap: u64,
    shadow: u64,
) -> Result<(), Error> {
    // SAFETY: the caller's contract.
    unsafe { mpio.request(MSG_HOST_BRIDGE, trap_arguments(trap, shadow)) }.map(drop)
}

/// Close the host bridge hand-over of the IOD (`0x30`, arg4 = finalize).
pub fn finalize_host_bridges<A: SmnAccess>(mpio: &Proxy<'_, A>) -> Result<(), Error> {
    let mut args = [0; ARGUMENT_COUNT];
    args[4] = FINALIZE;
    // SAFETY: no addresses in the arguments.
    unsafe { mpio.request(MSG_HOST_BRIDGE, args) }.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_info_packs_the_vendor_fields() {
        let pei = Device {
            bdf: None,
            iohc: 1,
            port: 2,
            device_type: DeviceType::Type3,
            version: Version::V2,
        };
        assert_eq!(pei.info(), 0x2321_0000);
        let dxe = Device {
            bdf: Some((0x42, 0, 0)),
            ..pei
        };
        assert_eq!(dxe.info(), 0x2321_4200);
        let rch = Device {
            bdf: Some((0x7f, 1, 2)),
            iohc: 6,
            port: 3,
            device_type: DeviceType::Type2,
            version: Version::Rch,
        };
        assert_eq!(rch.info(), 0x1236_7f0a);
        assert_eq!(
            trap_arguments(0x1_2345_0000, 0x7654_0000),
            [0x2345_0000, 1, 0x7654_0000, 0, 0, 0]
        );
    }
}
