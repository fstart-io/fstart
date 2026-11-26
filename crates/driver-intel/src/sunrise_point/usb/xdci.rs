// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native skylake/xdci.c host-only shutdown, before canonical PCI probing.
//! SHA256 f75934601cd76ea75ede280cdf258c2f3adb191f2446d123bee75d355aaa7f7b.
use super::{
    DeviceMode, Error, Prepared,
    registers::{Command, Registers, Status},
};
use crate::{
    generic::temporary_bar::{self, TemporaryBar},
    sunrise_point::pcr::{Pcr, Port},
};
use embedded_hal::delay::DelayNs;
use fstart_core::mmio::{MmioReadOnly as RO, MmioReadWrite as RW};
use fstart_pci::EcamDevice;
use tock_registers::{
    interfaces::{ReadWriteable, Readable},
    register_bitfields, register_structs,
};
/// This reserved UC aperture is borrowed sequentially, never concurrently
/// with the SATA temporary subwindow FC900000..FC902000.
const TEMP_BASE: usize = 0xfc80_0000;
register_bitfields![u32,
    Role [
        ID OFFSET(20) NUMBITS(1) [],
        SOFTWARE OFFSET(21) NUMBITS(1) []
    ],
    Host [ ACTIVE OFFSET(29) NUMBITS(1) [] ],
    Hibernation [ ENABLE OFFSET(1) NUMBITS(1) [] ],
    Suspend [ ENABLE OFFSET(6) NUMBITS(1) [] ],
    Pipe [ UX_IN_PX OFFSET(27) NUMBITS(1) [] ],
    PowerGate [ ENABLE OFFSET(0) NUMBITS(6) [] ],
    Shadow [ DISABLE OFFSET(8) NUMBITS(1) [] ]
];
register_bitfields![u16,
    D3Request [ VALUE OFFSET(0) NUMBITS(2) [] ], D3Status [ VALUE OFFSET(8) NUMBITS(4) [] ],
    Clock [ TRUNK OFFSET(2) NUMBITS(1) [] ],
    Cpge [
        DISABLE OFFSET(1) NUMBITS(1) [],
        ENABLE OFFSET(2) NUMBITS(1) []
    ],
    Power [
        STATE OFFSET(0) NUMBITS(2) [],
        PME_STATUS OFFSET(15) NUMBITS(1) []
    ]
];
register_structs! {
    DeviceRegisters {
        (0 => _reserved0), (0xc110 => control:RW<u32,Hibernation::Register>), (0xc114 => _reserved1),
        (0xc200 => usb2:RW<u32,Suspend::Register>), (0xc204 => _reserved2),
        (0xc2c0 => usb3:RW<u32,Pipe::Register>), (0xc2c4 => _reserved3),
        (0x10f810 => status:RO<u16,D3Status::Register>), (0x10f812 => _reserved4),
        (0x10f818 => request:RW<u16,D3Request::Register>), (0x10f81a => _reserved5), (0x10f81c => @END),
    },
    RoleRegisters {
        (0 => _reserved0), (0x80d8 => control:RW<u32,Role::Register>),
        (0x80dc => status:RO<u32,Host::Register>), (0x80e0 => @END),
    }
}
impl From<temporary_bar::Error> for Error {
    fn from(error: temporary_bar::Error) -> Self {
        match error {
            temporary_bar::Error::Readback => Self::Readback,
            _ => Self::Resource,
        }
    }
}
fn force_host(device: EcamDevice, delay: &mut impl DelayNs) -> Result<(), Error> {
    let lease = unsafe { TemporaryBar::<Registers>::bind(device, 0x10, TEMP_BASE, 0x10000) }?;
    let result = (|| {
        let controller = lease.registers();
        if controller.caplength.get() != 0x80
            || controller.command.is_set(Command::RUN)
            || controller.command.is_set(Command::RESET)
            || !controller.status.is_set(Status::HALTED)
            || controller.status.is_set(Status::NOT_READY)
        {
            return Err(Error::ActiveHost);
        }
        // Both blocks inhabit the same known 64-KiB lease. Keep this borrow
        // scoped until explicit resource cleanup; it cannot escape this call.
        let role = unsafe { &*(TEMP_BASE as *const RoleRegisters) };
        role.control.modify(Role::SOFTWARE::SET + Role::ID::CLEAR);
        for sample in 0..=1000 {
            if role.status.is_set(Host::ACTIVE) {
                return Ok(());
            }
            if sample != 1000 {
                delay.delay_us(100);
            }
        }
        Err(Error::HostModeTimeout)
    })();
    lease.close()?;
    result
}
/// # Safety
/// Actual cold PCH-H/LP and exclusive mapped PCR/ECAM/PWRM plus the reserved UC
/// FC800000..FCA00000 aperture. No retained USB/DMA/OS/AP/SMM owner. This
/// deliberately supports host-only policy, never pretends to enable xDCI.
pub(crate) unsafe fn prepare(
    pcr: &Pcr,
    pmc: &crate::sunrise_point::pmc::PmcRegisters,
    delay: &mut impl DelayNs,
) -> Result<Prepared, Error> {
    let lpc = EcamDevice::new(0, 0x1f, 0);
    let silicon = crate::sunrise_point::hsio::Silicon::read(&lpc)?;
    let kind = silicon.kind();
    let device = EcamDevice::new(0, 0x14, 1);
    if !device.is_present() {
        return Ok(Prepared {
            device_mode: DeviceMode::Absent,
            _bsp_only: core::marker::PhantomData,
        });
    }
    let host = EcamDevice::new(0, 0x14, 0);
    if device.vendor_id() != 0x8086 || host.vendor_id() != 0x8086 || host.read32(8) >> 8 != 0x0c0330
    {
        return Err(Error::Device);
    }
    // Reject existing resource ownership before sideband/role changes.
    if device.read16(4) & 7 != 0
        || host.read16(4) & 7 != 0
        || device.memory_bar(0x10).is_some_and(|bar| bar != 0)
        || host.memory_bar(0x10).is_some_and(|bar| bar != 0)
    {
        return Err(Error::Resource);
    }
    pcr.register::<PowerGate::Register>(Port::Otg, 0x1d0)?
        .modify(PowerGate::ENABLE.val(0x3f));
    pcr.register16::<Clock::Register>(Port::Otg, 0xa2)?
        .modify(Clock::TRUNK::SET);
    force_host(host, delay)?;
    let lease =
        unsafe { TemporaryBar::<DeviceRegisters>::bind(device, 0x10, TEMP_BASE, 0x200000) }?;
    let result = (|| {
        let registers = lease.registers();
        registers.control.modify(Hibernation::ENABLE::SET);
        registers.usb2.modify(Suspend::ENABLE::SET);
        registers.usb3.modify(Pipe::UX_IN_PX::SET);
        registers.request.modify(D3Request::VALUE.val(3));
        for sample in 0..=10000 {
            if registers.status.read(D3Status::VALUE) == 15 {
                return Ok(());
            }
            if sample != 10000 {
                delay.delay_us(10);
            }
        }
        Err(Error::DeviceD3Timeout)
    })();
    // Neither timeout nor failed cleanup permits hiding the device. The
    // controller's irreversible suspend requests are not hardware rollback.
    lease.close()?;
    result?;
    unsafe { device.register::<u16, Cpge::Register>(0xa2) }
        .modify(Cpge::DISABLE::CLEAR + Cpge::ENABLE::SET);
    unsafe { device.register::<u16, Power::Register>(0x84) }
        .modify(Power::STATE.val(3) + Power::PME_STATUS::CLEAR);
    if device.read16(0x84) & 3 != 3 {
        return Err(Error::Readback);
    }
    let shadow_offset = match kind {
        crate::sunrise_point::dmi::Kind::H => 0x21c,
        crate::sunrise_point::dmi::Kind::Lp => 0x61c,
    };
    let shadow = pcr.register::<Shadow::Register>(Port::Psf2, shadow_offset)?;
    shadow.modify(Shadow::DISABLE::SET);
    if !shadow.is_set(Shadow::DISABLE) || device.is_present() {
        return Err(Error::Readback);
    }
    // PMC NST_PG_FDIS1[24] is LP B0+ ONLY, never PCH-H.
    if kind == crate::sunrise_point::dmi::Kind::Lp && lpc.read8(8) >= 0x10 {
        pmc.non_static_disable
            .modify(crate::sunrise_point::pmc::NonStaticDisable::XDCI::SET);
        if !pmc
            .non_static_disable
            .is_set(crate::sunrise_point::pmc::NonStaticDisable::XDCI)
        {
            return Err(Error::Readback);
        }
    }
    Ok(Prepared {
        device_mode: DeviceMode::Disabled,
        _bsp_only: core::marker::PhantomData,
    })
}
