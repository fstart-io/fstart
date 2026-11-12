// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Picasso on the noncar flow: the reset image's layout, the host
//! plan, the display and the tables of the SoC whose silicon init is
//! `fstart_driver_amd::soc::picasso`.
//!
//! One PSP-loaded stage runs the native silicon init, MP/SMM, PCI and
//! ACPI before handing off to the selected payload.
pub mod acpi;
use crate::noncar::{
    self, Ctx, Devices, Silicon, Stop, Tables,
    layout::{Layout, Region},
};
pub use fstart_driver_amd::soc::picasso::{
    Config, FchConfig, GpioInterrupt, I2cBus, I2cDevice, Runtime,
};
use fstart_driver_amd::{
    ccx::picasso as cpu,
    soc::picasso::{Picasso, State, UART_BASES},
};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

/// The console on FCH UART `uart`.
pub const fn console(uart: u8) -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Mmio {
            base: UART_BASES[uart as usize] as u64,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 48_000_000,
        baud_rate: 115200,
    }
}

/// The reset image in DRAM. The APOB (`0x02001000`), the PSP shared memory
/// of a PSP verstage (`0x02011000..0x02019000`) and the image are fixed by
/// the PSP directories.
pub const LAYOUT: Layout = Layout {
    early: Region::new(0x0200_0000, 0x140_0000),
    page_tables: 0x0202_0000,
    data: Region::new(0x0203_0000, 0x1d_0000),
    heap: Region::new(0x0220_0000, 0x80_0000),
    stack: Region::new(0x02f0_0000, 0x10_0000),
    // Leave room for the Rust interpreter and graphical UEFI, plus the
    // reserved 16-bit entry at the image's end.
    image: Region::new(0x0300_0000, 0x40_0000),
    flash: Region::new(0xff00_0000, 0x100_0000),
    ffs: Some(Region::new(0xffb8_0000, 0x29_f000)),
}
.check();

#[cfg(feature = "host")]
impl noncar::host::HostSoc for Picasso {
    const POLICY: noncar::host::Policy = noncar::host::Policy {
        feature: "bundle-picasso",
        payloads: true,
        smm: true,
        release_only: true,
        image_config: noncar::host::ImageConfig::Packed {
            // Reserve the full uncompressed size before the fixed FFS window.
            reset_offset: Some(
                (LAYOUT.ffs.unwrap().base - LAYOUT.flash.base - LAYOUT.image.size) as u32,
            ),
            psp: fstart_image_build::amd::assembly::PspOptions {
                compress_reset: true,
                spi: None,
            },
        },
    };
    fn check(config: &Config) -> Result<(), &'static str> {
        (config.fch.console_uart < UART_BASES.len() as u8)
            .then_some(())
            .ok_or("Picasso console UART index exceeds hardware table")
    }
}

/// `DISABLE_SPI_FLASH_ROM_SHARING`: stop sharing the SPI ROM with the EC.
/// Boards that need it call it once their early pads are set.
#[cfg(all(feature = "bundle", target_os = "none"))]
impl noncar::Stage<Picasso, noncar::FchUp<Picasso>> {
    pub fn disable_rom_sharing(&self) {
        fstart_driver_amd::fch::sandstone::legacy::disable_rom_sharing(self.hw().fch.bus());
    }
}

impl noncar::Soc for Picasso {
    const LAYOUT: Layout = LAYOUT;
    const APOB: Region = Region::new(0x0200_1000, 0x1_0000);
    const BIOS_DIRECTORY: fstart_amdfw_format::efs::BiosDirectorySlot =
        fstart_amdfw_format::efs::BiosDirectorySlot::Family17hModel10;
    /// 8 MiB of TSEG.
    const TSEG_SIZE: u32 = 8 << 20;
    /// The topology decoder's envelope: 2 CCXs, 4 cores, 2 threads.
    const MAX_CPUS: u16 = 16;
    type Smbios<'a> = crate::acpi::SmbiosIdentity<'a>;
    type Acpi = crate::acpi::BoardAcpi;

    /// The eMMC controller if it is set up, the FCH GPIO controller and the
    /// board's I2C controllers with the devices behind them.
    fn acpi_devices(
        config: &Config,
        state: &State,
    ) -> Result<(Devices, alloc::vec::Vec<u8>), &'static str> {
        let devices = state
            .emmc
            .into_iter()
            .map(|_| acpi::emmc())
            .collect::<Result<_, _>>()
            .map_err(|_| "eMMC ACPI")?;
        let aml = acpi::devices(config.i2c, state.emmc.is_some()).map_err(|_| "ACPI devices")?;
        Ok((devices, aml))
    }

    /// The GPU's video ROM, if this part has a GPU: its framebuffer for the
    /// payload, and VFCT and `_ROM` for the OS.
    fn init_display(
        silicon: &mut Silicon<Self>,
        ctx: &Ctx<'_, Self>,
        pci: &fstart_pci::PciEcam,
    ) -> Result<(), Stop> {
        if let Some((uma, kind)) = silicon.state.uma {
            extern crate alloc;
            let address = pci
                .devices()
                .find(|&a| {
                    pci.device(a)
                        .is_some_and(|d| d.read8(0x0b) == 3 && d.read16(0) == 0x1002)
                })
                .ok_or("VGA device")?;
            let gpu = pci.device(address).ok_or("VGA device")?;
            let id = gpu.read32(0);
            // Raven2 can retain Picasso's PCI ID; its ROM is selected by
            // the CPU/SMU silicon identity, not by that PCI ID alone.
            let rom_device = if id == 0x15d8_1002 && kind == cpu::Silicon::Raven2 {
                0x15dd
            } else {
                (id >> 16) as u16
            };
            let name = alloc::format!("pci{:04x},{rom_device:04x}.rom", id as u16);
            fstart_log::info!("picasso: VGA ROM {}", name.as_str());
            let rom = fstart_core::services::ffs_context::read_verified_asset(&name)
                .ok_or("verified VGA ROM")?;
            let mut windows: heapless::Vec<_, 8> = pci
                .assigned_memory_bars()
                .filter(|(a, _)| *a == address)
                .map(|(_, w)| w)
                .collect();
            windows
                .push(fstart_pci::PciWindow::mmio(uma.base, uma.size))
                .map_err(|_| "VGA resource windows")?;
            // SAFETY: allocated VGA BARs and reserved UMA, BSP only, before
            // any payload/display driver runs. Only an authenticated ROM is used.
            let display = unsafe {
                fstart_driver_vbios::initialize(pci, address, rom, rom_device, &windows, &ctx.clock)
            }
            .map_err(|e| e.name())?;
            silicon.framebuffer = Some(display.framebuffer);
            // amdgpu's APU path consumes VFCT, not the standard _ROM method.
            // Match the actual function, even when its ROM uses a PCIR alias.
            silicon.tables.push(
                fstart_acpi::vfct::build_vfct(
                    [0; 16],
                    &[fstart_acpi::vfct::VbiosImage {
                        segment: address.segment(),
                        bus: address.bus(),
                        device: address.device(),
                        function: address.function(),
                        vendor_id: id as u16,
                        device_id: (id >> 16) as u16,
                        subsystem_vendor_id: gpu.read16(0x2c),
                        subsystem_id: gpu.read16(0x2e),
                        // No VFCT image revision is supplied by this producer;
                        // follow coreboot's zero, not the PCI revision ID.
                        revision: 0,
                        rom: &display.rom,
                    }],
                )
                .map_err(|_| "VGA VFCT")?,
            );
            let path = pci.device_path(address).ok_or("VGA PCI path")?;
            // Picasso has one PCI root, PCI0. The bridge/function path is
            // discovered, not assumed to be a particular bus or internal port.
            silicon.aml.extend(
                crate::acpi::video_rom_aml(0, &path, &display.rom).map_err(|_| "VGA ACPI ROM")?,
            );
        }
        Ok(())
    }

    #[cfg(all(feature = "bundle", target_os = "none"))]
    fn tables(
        ctx: &Ctx<'_, Self>,
        silicon: &mut Silicon<Self>,
        pci: &fstart_pci::PciEcam,
        smbios: &crate::acpi::SmbiosIdentity<'_>,
        acpi: crate::acpi::BoardAcpi,
    ) -> Tables {
        noncar::tables(ctx, silicon, pci, smbios, acpi)
    }
    #[cfg(not(all(feature = "bundle", target_os = "none")))]
    fn tables(
        _: &Ctx<'_, Self>,
        _: &mut Silicon<Self>,
        _: &fstart_pci::PciEcam,
        _: &crate::acpi::SmbiosIdentity<'_>,
        _: crate::acpi::BoardAcpi,
    ) -> Tables {
        unreachable!("the tables are the stage's")
    }

    #[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
    fn peripherals(ctx: &Ctx<'_, Self>, config: &Config) -> noncar::Peripherals {
        config
            .i2c
            .iter()
            .flat_map(|bus| uefi::i2c_peripherals(ctx, bus))
            .collect()
    }
}

impl noncar::DieSoc for Picasso {
    /// FP5 is a BGA package with two channels.
    const SMBIOS: noncar::SmbiosFacts = noncar::SmbiosFacts {
        socket_upgrade: 0x06,
        max_dimm_mib: 32 * 1024,
        channels_per_die: 2,
    };
    const ACPI: &'static crate::acpi::SocAcpi = &acpi::ACPI;

    fn ioapic(_: &State, _: u8) -> u32 {
        Picasso::IOAPIC
    }
}

/// Board I2C devices handed to CrabEFI.
#[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
mod uefi {
    extern crate alloc;
    use super::{GpioInterrupt, I2cBus, Picasso};
    use crate::noncar::{Ctx, Peripherals};
    use alloc::{boxed::Box, rc::Rc};
    use core::cell::RefCell;
    use embedded_hal_bus::i2c::RcDevice;
    use fstart_payload::crabefi::peripheral::{
        self, I2cPeripheral, InputLine, Interrupt, InterruptLatch, Peripheral,
    };

    /// A pad's latched interrupt status.
    struct Latch(
        fstart_driver_amd::fch::gpio::Gpio<fstart_driver_amd::fch::NativeBus>,
        u8,
    );

    impl InterruptLatch for Latch {
        fn take(&mut self) -> bool {
            self.0.take_interrupt_status(self.1).unwrap_or(false)
        }
    }

    fn interrupt(ctx: &Ctx<'_, Picasso>, config: GpioInterrupt) -> Option<Interrupt> {
        let gpio = ctx.hw.gpio();
        Some(match config {
            GpioInterrupt::Level { pin, active_low } => Interrupt::Level {
                line: InputLine::new(gpio.input(pin).ok()?),
                active_low,
            },
            GpioInterrupt::Edge { pin, .. } => Interrupt::Latched(Box::new(Latch(gpio, pin))),
        })
    }

    /// The devices on `bus` that the payload can drive, sharing the
    /// controller. A controller without such devices is left alone.
    pub(super) fn i2c_peripherals(ctx: &Ctx<'_, Picasso>, bus: &I2cBus) -> Peripherals {
        let devices = bus
            .devices
            .iter()
            .filter(|device| peripheral::i2c_supported(device.ids.iter().copied()));
        if devices.clone().next().is_none() {
            return Peripherals::new();
        }
        // SAFETY: configured pads, decoded UC controller, no other user
        // before payload/OS handoff. CrabEFI owns the bus thereafter.
        let controller = match unsafe {
            fstart_driver_amd::fch::i2c::controller::<Picasso, _>(
                bus.controller,
                &bus.timing,
                ctx.clock,
            )
        } {
            Ok(controller) => Rc::new(RefCell::new(controller)),
            Err(_) => {
                fstart_log::warn!("picasso: I2C{} unavailable", bus.controller);
                return Peripherals::new();
            }
        };
        devices
            .map(|device| {
                Peripheral::I2c(I2cPeripheral {
                    ids: device.ids.iter().map(|&id| id.into()).collect(),
                    bus: peripheral::I2cBus::new(RcDevice::new(controller.clone())),
                    address: device.address,
                    interrupt: device.interrupt.and_then(|config| interrupt(ctx, config)),
                    hid_descriptor_register: device.hid_descriptor_register,
                })
            })
            .collect()
    }
}
