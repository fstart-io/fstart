// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Picasso on the noncar flow.
//!
//! One PSP-loaded stage runs the native silicon init, MP/SMM, PCI and
//! ACPI before handing off to the selected payload.
pub mod acpi;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod init;
use crate::noncar::{
    self, Ctx, Silicon, Stop,
    layout::{Layout, Region},
};
use fstart_driver_amd::{
    ccx::{
        picasso::{self as cpu, Identity},
        tsc::Tsc,
    },
    fch::{
        Fch, NativeBus, espi,
        gpio::{Gpio, Pad, Pull},
        sandstone::preinit::SpiConfig,
    },
    nbio::picasso as nbio,
    soc::{
        Soc as _,
        picasso::{self as soc, EARLY_AOAC, Picasso, UART_AOAC, UART_BASES},
    },
};
use fstart_driver_amd::{
    dxio::Descriptor,
    fch::{
        emmc,
        sandstone::{self, usb},
    },
};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
#[cfg(all(feature = "bundle", target_os = "none"))]
pub use init::{Dxio, Init};

/// A board's Picasso configuration: how the board wires the SoC's blocks.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub fch: FchConfig,
    pub pcie: &'static [Descriptor],
    pub usb: usb::Config,
    /// The eMMC controller, if the board can have eMMC fitted.
    pub emmc: Option<emmc::Config>,
    pub graphics: nbio::gfx::Config,
    /// FCH I2C controllers and their devices, for the UEFI payload to
    /// drive and the OS to find in ACPI.
    pub i2c: &'static [I2cBus],
}

/// An FCH I2C controller and the devices on it.
#[derive(Debug, Clone, Copy)]
pub struct I2cBus {
    pub controller: usize,
    pub timing: fstart_driver_amd::fch::i2c::Timing,
    pub devices: &'static [I2cDevice],
}

/// An I2C device, possibly one of several fitted alternatives.
#[derive(Debug, Clone, Copy)]
pub struct I2cDevice {
    /// ACPI hardware and compatible IDs, which select the payload's driver.
    pub ids: &'static [&'static str],
    pub address: u8,
    pub interrupt: Option<GpioInterrupt>,
    /// HID descriptor register of a HID-over-I2C device.
    pub hid_descriptor_register: Option<u16>,
}

/// A GPIO interrupt as its pad is configured.
#[derive(Debug, Clone, Copy)]
pub enum GpioInterrupt {
    /// Level-triggered, read from the pad.
    Level { pin: u8, active_low: bool },
    /// Edge-triggered, latched in the pad's interrupt status.
    Edge { pin: u8, falling: bool },
}

/// The Sandstone FCH setup before and right after the console.
#[derive(Debug, Clone, Copy)]
pub struct FchConfig {
    /// The FCH UART of the console (0-3).
    pub console_uart: u8,
    /// SPI ROM read mode and speeds.
    pub spi: SpiConfig,
    /// eSPI link to the EC and its pads.
    pub espi: Option<(espi::Config, &'static [Pad])>,
}

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

impl noncar::Soc for Picasso {
    const NAME: &'static str = "picasso";
    const CPU_NAME: &'static str = "native-picasso";
    const LAYOUT: Layout = LAYOUT;
    const ECAM: u64 = soc::ECAM;
    const ECAM_BUSES: u16 = soc::ECAM_BUSES as u16;
    /// BIOS directory entry 0x61.
    const PSP: fstart_driver_amd::psp::Variant = fstart_driver_amd::psp::Variant::Rv;
    const APOB: Region = Region::new(0x0200_1000, 0x1_0000);
    const BIOS_DIRECTORY: fstart_amdfw_format::efs::BiosDirectorySlot =
        fstart_amdfw_format::efs::BiosDirectorySlot::Family17hModel10;
    /// 8 MiB of TSEG.
    const TSEG_SIZE: u32 = 8 << 20;
    /// The topology decoder's envelope: 2 CCXs, 4 cores, 2 threads.
    const MAX_CPUS: u16 = 16;
    type Config = Config;
    type Identity = Identity;
    /// The reserved UMA when this part has a GPU.
    type State = Option<(fstart_driver_amd::apob::Hole, cpu::Silicon)>;

    /// The console: ACPI MMIO decode, legacy decode, AOAC power of the
    /// early devices and the console UART, its pads.
    fn fch_pre_init(
        fch: &Fch<NativeBus>,
        config: &Config,
        clock: Option<Tsc>,
    ) -> Result<(), &'static str> {
        let mut delay = clock.ok_or("TSC frequency")?;
        let uart = config.fch.console_uart;
        fch.enable_legacy_decode();
        // Power all common early devices before waiting, then the UART.
        fch.power_devices(EARLY_AOAC, 10000, &mut delay)
            .map_err(|_| "AOAC power")?;
        let aoac = *UART_AOAC.get(usize::from(uart)).ok_or("console UART")?;
        fch.power_devices(&[aoac], 10000, &mut delay)
            .map_err(|_| "AOAC power")?;
        // Range_0 comes out of reset enabled; clear legacy UART aliases.
        fch.clear_legacy_uart_decode();
        let pins = match uart {
            0 => [
                Pad::native(138, 0, Pull::None),
                Pad::native(136, 0, Pull::None),
            ],
            1 => [
                Pad::native(143, 0, Pull::None),
                Pad::native(141, 0, Pull::None),
            ],
            2 => [
                Pad::native(137, 1, Pull::None),
                Pad::native(135, 1, Pull::None),
            ],
            _ => [
                Pad::native(140, 1, Pull::None),
                Pad::native(142, 1, Pull::None),
            ],
        };
        // SAFETY: as the flow's FCH: BSP only, before the console.
        let fch = Fch::new(unsafe { NativeBus::new() }, fch.acpimmio());
        Gpio::new(fch, Picasso::GPIO_BANK)
            .configure(&pins, &[])
            .map_err(|_| "console pads")
    }

    fn identify(_: &Fch<NativeBus>, _: &Config, mhz: u32, _: u8) -> Result<Identity, &'static str> {
        fstart_log::info!("fstart: AMD Picasso cold boot, TSC {} MHz", mhz);
        let (signature, _, _, _) = fstart_arch::x86::cpuid(1);
        let (_, extended_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        Identity::from_cpuid(signature, extended_ebx, 0).map_err(|_| "CPU identification")
    }

    fn after_resources(_: &mut Silicon<Self>, ctx: &Ctx<'_, Self>) {
        ctx.fch.acpi_ports(sandstone::ACPI_PORTS);
    }

    fn ready_to_boot(
        silicon: &mut Silicon<Self>,
        ctx: &Ctx<'_, Self>,
        pci: &fstart_pci::PciEcam,
    ) -> Result<(), Stop> {
        let devices: heapless::Vec<_, 256> = pci.devices().collect();
        nbio::late::after_enumeration(&ctx.dies[0].smn(), &ctx.dies[0].pci(), &devices);
        if let Some((uma, kind)) = silicon.state {
            extern crate alloc;
            let address = devices
                .iter()
                .copied()
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
                .push(fstart_pci::PciWindow {
                    kind: fstart_pci::PciWindowKind::Mmio,
                    base: uma.base,
                    size: uma.size,
                    prefetchable: false,
                })
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

    #[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
    fn peripherals(ctx: &Ctx<'_, Self>, config: &Config) -> noncar::Peripherals {
        config
            .i2c
            .iter()
            .flat_map(|bus| uefi::i2c_peripherals(ctx, bus))
            .collect()
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
    use fstart_stage::crabefi::peripheral::{
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
        let gpio = ctx.gpio();
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
