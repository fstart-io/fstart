// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point private-configuration-register transport.
//!
//! Derived from coreboot common/block/pcr/pcr.c and p2sb/p2sblib.c.
//! PCR writes require a same-dword completion read, not just an MMIO fence.
//! The tock transport below adds that requirement to existing MMIO accessors.

use fstart_core::mmio::MmioRegister;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{RegisterLongName, UIntLike, register_bitfields};

pub const DEFAULT_BASE: u32 = 0xfd00_0000;
pub const WINDOW_SIZE: usize = 0x100_0000;

/// Source `soc/pcr_ids.h` ports used by PCH-H bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Port {
    Psth = 0x89,
    CsmePsf = 0x8f,
    Modphy3 = 0xa8,
    Modphy2 = 0xa9,
    Gpio3 = 0xac,
    Gpio2 = 0xad,
    Gpio1 = 0xae,
    Gpio0 = 0xaf,
    Modphy4 = 0xb0,
    Dci = 0xb8,
    Psf1 = 0xba,
    Psf2 = 0xbb,
    Psf3 = 0xbc,
    Psf4 = 0xbd,
    Ish = 0xbf,
    Scs = 0xc0,
    Rtc = 0xc3,
    Itss = 0xc4,
    Smbus = 0xc6,
    Lpc = 0xc7,
    Spf = 0xc9,
    Usb2 = 0xca,
    SerialIo = 0xcb,
    Fia = 0xcf,
    Dsp = 0xd7,
    Icc = 0xdc,
    Spa = 0xe0,
    Spb = 0xe1,
    Spc = 0xe2,
    Spd = 0xe3,
    Spe = 0xe4,
    Otg = 0xe5,
    Modphy1 = 0xe9,
    Modphy0 = 0xea,
    OpiPhy = 0xec,
    EspiSpi = 0xee,
    Dmi = 0xef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidBase,
    UnalignedOffset,
    WrongFunction,
    Unavailable,
    Readback,
}

/// A bound 16-MiB SBREG window, distinct from PCI ECAM.
#[derive(Debug, Clone, Copy)]
pub struct Pcr {
    base: usize,
}
impl Pcr {
    /// # Safety
    /// The complete, aligned 16-MiB window must remain mapped and enabled for
    /// as long as this value or any returned register is used. The caller owns
    /// the accessed ports/registers and serializes competing firmware agents.
    /// This constructor does not enable the P2SB BAR or reserve its resources.
    pub unsafe fn at(base: usize) -> Result<Self, Error> {
        if base == 0 || base & (WINDOW_SIZE - 1) != 0 || base.checked_add(WINDOW_SIZE - 1).is_none()
        {
            return Err(Error::InvalidBase);
        }
        Ok(Self { base })
    }

    pub fn register<R: RegisterLongName>(
        &self,
        port: Port,
        offset: u16,
    ) -> Result<PcrRegister<R>, Error> {
        self.register_width(port, offset)
    }

    pub fn register8<R: RegisterLongName>(
        &self,
        port: Port,
        offset: u16,
    ) -> Result<PcrRegister<R, u8>, Error> {
        self.register_width(port, offset)
    }

    pub fn register16<R: RegisterLongName>(
        &self,
        port: Port,
        offset: u16,
    ) -> Result<PcrRegister<R, u16>, Error> {
        self.register_width(port, offset)
    }

    fn register_width<R: RegisterLongName, T: UIntLike>(
        &self,
        port: Port,
        offset: u16,
    ) -> Result<PcrRegister<R, T>, Error> {
        if usize::from(offset) & (core::mem::size_of::<T>() - 1) != 0 {
            return Err(Error::UnalignedOffset);
        }
        let address = self.base + ((port as usize) << 16) + usize::from(offset);
        Ok(PcrRegister {
            // SAFETY: only the naturally aligned u8/u16/u32 entry points above
            // construct registers; every span stays within this owned PID page.
            inner: unsafe { MmioRegister::new(address) },
            completion: unsafe { MmioRegister::new(address & !3) },
        })
    }
}

/// IOSF register: every width uses the native same-dword completion read.
pub struct PcrRegister<R: RegisterLongName = (), T: UIntLike = u32> {
    inner: MmioRegister<T, R>,
    completion: MmioRegister<u32>,
}
impl<R: RegisterLongName, T: UIntLike> Readable for PcrRegister<R, T> {
    type T = T;
    type R = R;
    fn get(&self) -> T {
        self.inner.get()
    }
}
impl<R: RegisterLongName, T: UIntLike> Writeable for PcrRegister<R, T> {
    type T = T;
    type R = R;
    fn set(&self, value: T) {
        self.inner.set(value);
        // A CPU fence or a narrow read cannot replace posted completion.
        let _ = self.completion.get();
    }
}

register_bitfields![u16,
    PciCommand [
        MEMORY OFFSET(1) NUMBITS(1) [],
        BUS_MASTER OFFSET(2) NUMBITS(1) []
    ],
    RequesterId [
        FUNCTION OFFSET(0) NUMBITS(3) [],
        DEVICE OFFSET(3) NUMBITS(5) [],
        BUS OFFSET(8) NUMBITS(8) []
    ]
];

pub const HPET_REQUESTER_ID: u16 = 0xfa78;
pub const IOAPIC_REQUESTER_ID: u16 = 0xfaf8;

/// Native early VT-d virtual originators; P2SB must already be visible.
/// This configures requester IDs only, not translation or DMA protection.
pub fn configure_vtd_originators(device: &EcamDevice) -> Result<(), Error> {
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 1
    {
        return Err(Error::WrongFunction);
    }
    if device.vendor_id() != 0x8086 {
        return Err(Error::Unavailable);
    }
    // Native PCH_P2SB_HBDF=0x70 and IBDF=0x6c, exact 16-bit accesses.
    let hpet = unsafe { device.register::<u16, RequesterId::Register>(0x70) };
    let ioapic = unsafe { device.register::<u16, RequesterId::Register>(0x6c) };
    hpet.write(
        RequesterId::BUS.val(250) + RequesterId::DEVICE.val(15) + RequesterId::FUNCTION.val(0),
    );
    ioapic.write(
        RequesterId::BUS.val(250) + RequesterId::DEVICE.val(31) + RequesterId::FUNCTION.val(0),
    );
    if hpet.get() != HPET_REQUESTER_ID || ioapic.get() != IOAPIC_REQUESTER_ID {
        return Err(Error::Readback);
    }
    Ok(())
}

/// Program a visible P2SB function, using the existing typed ECAM access path.
///
/// `device` must be the stage-bound 00:1f.1 function. PCIEXBAR/ECAM must already
/// be enabled. BAR resources remain platform-owned; this function neither maps
/// them nor hides/locks the bridge. The upper BAR is explicitly zeroed.
pub fn enable_p2sb(device: &EcamDevice, base: u32) -> Result<(), Error> {
    if base == 0 || base as usize & (WINDOW_SIZE - 1) != 0 {
        return Err(Error::InvalidBase);
    }
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 1
    {
        return Err(Error::WrongFunction);
    }
    if device.vendor_id() != 0x8086 {
        return Err(Error::Unavailable);
    }
    // SAFETY: the validated function and caller-owned stable ECAM mapping.
    let command = unsafe { device.register::<u16, PciCommand::Register>(0x04) };
    let old = command.get();
    command.modify(PciCommand::MEMORY::CLEAR);
    unsafe {
        device.register::<u32, ()>(0x10).set(base);
        device.register::<u32, ()>(0x14).set(0);
    }
    command.set(old | PciCommand::MEMORY::SET.value | PciCommand::BUS_MASTER::SET.value);
    // HPTC: enable HPET decode at the default address, exact byte width.
    unsafe { device.register::<u8, ()>(0x60).set(0x80) };
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    extern crate std;
    use super::*;
    use core::ptr::NonNull;
    use fstart_pci::{PciEcam, PciRoot, PciRootInfo};
    use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
    use std::vec;
    use std::vec::Vec;

    pub struct Image {
        pointer: NonNull<u8>,
    }
    impl Image {
        pub fn new() -> Self {
            let layout = Layout::from_size_align(WINDOW_SIZE, WINDOW_SIZE).unwrap();
            let pointer = unsafe { alloc_zeroed(layout) };
            let pointer = NonNull::new(pointer).unwrap_or_else(|| handle_alloc_error(layout));
            Self { pointer }
        }
        pub fn pcr(&self) -> Pcr {
            unsafe { Pcr::at(self.pointer.as_ptr() as usize).unwrap() }
        }
    }
    impl Drop for Image {
        fn drop(&mut self) {
            unsafe {
                dealloc(
                    self.pointer.as_ptr(),
                    Layout::from_size_align(WINDOW_SIZE, WINDOW_SIZE).unwrap(),
                );
            }
        }
    }

    pub fn ecam() -> (PciEcam, Vec<u32>) {
        let mut image = vec![0u32; (1 << 20) / 4];
        let pci = PciEcam::new([PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: image.as_mut_ptr() as u64,
                bus_start: 0,
                bus_end: 0,
            },
            [],
        )
        .unwrap()])
        .unwrap();
        (pci, image)
    }

    #[test]
    fn pcr_invalid_mapping_or_p2sb_geometry_fails_before_config_access() {
        for base in [0, 4, DEFAULT_BASE as usize + 4, usize::MAX] {
            assert!(matches!(unsafe { Pcr::at(base) }, Err(Error::InvalidBase)));
        }
        assert_eq!(
            enable_p2sb(&EcamDevice::new(0, 0x1f, 1), 0),
            Err(Error::InvalidBase)
        );
        assert_eq!(
            enable_p2sb(&EcamDevice::new(0, 0x1f, 1), DEFAULT_BASE + 4),
            Err(Error::InvalidBase)
        );
        assert_eq!(
            enable_p2sb(&EcamDevice::new(0, 0x1f, 0), DEFAULT_BASE),
            Err(Error::WrongFunction)
        );
    }
}
