// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Picasso on the noncar flow.
//!
//! One PSP-loaded stage runs the native silicon init, MP/SMM, PCI and
//! ACPI before handing off to the selected payload.
pub mod acpi;
use crate::noncar::{
    self, Board, Ctx, Silicon, Stop, Tp1,
    layout::{Layout, Region},
};
use crate::noncar::{mp, psp, smm};
use fstart_arch::x86::cpu::msr::{MCG_CAP, McgCap, MsrAccess};
use fstart_driver_amd::{
    ccx::{
        Thread,
        picasso::{self as cpu, Identity},
        tsc::{Clock, Tsc},
    },
    df::picasso as fabric,
    fch::{
        Fch, NativeBus, espi,
        gpio::{Gpio, Pad, Pull},
        sandstone::{
            legacy,
            preinit::{self, SpiConfig},
        },
    },
    nbio::picasso as nbio,
    soc::{
        Soc as _,
        picasso::{self as soc, EARLY_AOAC, Picasso, UART_AOAC, UART_BASES},
    },
};
use fstart_driver_amd::{
    dxio::{self, Descriptor},
    fch::{
        emmc,
        sandstone::{self, usb},
        smi::FchSmi,
    },
    smu::v10::power,
};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_log::Hex;
use tock_registers::interfaces::Readable;

/// A board's Picasso configuration.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub fch: FchConfig,
    pub pcie: &'static [Descriptor],
    /// Set by the board's CBI hook before SMU policy submission.
    pub power: Option<power::Config>,
    pub usb: usb::Config,
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
    /// GPIO tables programmed once the FCH is up, in order.
    pub early_gpios: &'static [&'static [Pad]],
    /// `DISABLE_SPI_FLASH_ROM_SHARING`: before the board's `fch_reset_done`.
    pub disable_rom_sharing: bool,
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
    early: Region::new(0x0200_0000, 0x120_0000),
    page_tables: 0x0202_0000,
    data: Region::new(0x0203_0000, 0x1d_0000),
    heap: Region::new(0x0220_0000, 0x80_0000),
    stack: Region::new(0x02f0_0000, 0x10_0000),
    image: Region::new(0x0300_0000, 0x20_0000),
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
            reset_offset: Some(0x98_0000),
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

    /// Cold silicon init in the native coreboot sequence, followed by the
    /// shared architectural MP and SMM installers.
    fn silicon_init<B: Board<Soc = Self>>(tp1: Tp1<'_, Self>) -> Result<Silicon<Self>, Stop> {
        let Tp1 {
            ctx,
            config,
            identity,
            memory,
            microcode,
        } = tp1;
        let mut config = *config;
        let Ctx { dies, msr, .. } = ctx;
        let die = &dies[0];
        let banks = msr.reg(MCG_CAP).read(McgCap::COUNT) as u8;
        if !(17..=32).contains(&banks) {
            return Err("CPU identification".into());
        }
        let (smn, df) = (&die.smn(), &die.df());
        let mut delay = ctx.clock;
        fabric::ensure_legacy_routes(die, memory.top_mem as u32).map_err(|_| "data fabric")?;
        fabric::early_init(df);
        let capabilities = nbio::early_init(smn, identity.raven2_cpuid, identity.am4, &mut delay)
            .map_err(|_| "NBIO early init")?;
        let (signature, _, _, _) = fstart_arch::x86::cpuid(1);
        let (_, extended_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        let identity = Identity::from_cpuid(signature, extended_ebx, capabilities.silicon_id)
            .map_err(|_| "CPU identification")?;
        nbio::psp_bar(smn).map_err(|_| "NBIO early init")?;
        cpu::bsp_init(msr);
        cpu::mca_init(msr, Thread::current()).map_err(|_| "CPU identification")?;
        nbio::dram_routes(die, memory.top_mem, memory.top_mem2).map_err(|_| "NBIO early init")?;
        fstart_log::info!(
            "picasso: silicon {}, TSEG {} + {}",
            Hex(u64::from(capabilities.silicon_id)),
            Hex(memory.tseg),
            Hex(u64::from(Self::TSEG_SIZE))
        );
        for entry in memory.e820.entries() {
            let (addr, size, kind) = (entry.addr, entry.size, entry.kind);
            fstart_log::info!(
                "picasso: e820 {}..{} type {}",
                Hex(addr),
                Hex(addr + size),
                kind
            );
        }
        fch_init(ctx.fch, &config.fch, &mut delay)?;
        B::fch_reset_done(&mut ctx.board(), &mut config);
        nbio::ioapic_decode(smn);
        fstart_driver_amd::nbio::iommu::init_bar(die, nbio::IOMMU_MMIO_BASE);
        static mut DXIO: dxio::picasso::Buffers = dxio::picasso::Buffers::new();
        static mut POWER: power::Pages = power::Pages::new();
        // SAFETY: BSP-only, once per boot. Both buffers remain in reserved
        // firmware BSS and are never reclaimed, including on failure.
        let topology = unsafe {
            dxio::picasso::train(
                die,
                &mut *core::ptr::addr_of_mut!(DXIO),
                config.pcie,
                dxio::picasso::Options {
                    power: dxio::picasso::PowerPolicy::Balanced,
                    am4: identity.am4,
                    silicon_id: capabilities.silicon_id,
                    graphics: capabilities.graphics,
                    hda_enabled: true,
                    acp_enabled: true,
                },
                &mut delay,
            )
        }
        .map_err(|_| "DXIO training")?;
        B::dxio_training_done(&mut ctx.board());
        fstart_log::info!("picasso: DXIO training complete");
        let mut notice = |message, status| {
            fstart_log::warn!(
                "SMU message {} returned {}",
                Hex(u64::from(message)),
                Hex(u64::from(status))
            )
        };
        // SAFETY: permanent coherent BSS pages, as above.
        unsafe {
            nbio::late::pre_enumeration(
                smn,
                &topology,
                &mut *core::ptr::addr_of_mut!(POWER),
                config.power.ok_or("board power policy")?,
                &mut delay,
                &mut notice,
            )
        }
        .map_err(|_| "SMU power policy")?;
        sandstone::alink_init(ctx.fch);
        ctx.fch.acpi_decode(sandstone::ACPI_PORTS);
        legacy::initialize(ctx.fch, config.fch.espi.is_some(), false);
        legacy::lpc_init(ctx.fch.bus());
        sandstone::clock_init(smn);
        // SAFETY: BSP owns the decoded fixed USB PHY registers.
        usb::initialize(
            smn,
            &mut unsafe { usb::NativePhy::new() },
            &config.usb,
            identity,
            capabilities.silicon_id,
            false,
            &mut delay,
        )
        .map_err(|_| "USB init")?;
        fabric::late_init(df, identity.raven2_cpuid);
        if let Some(emmc) = config.emmc {
            // SAFETY: BSP owns the decoded eMMC configuration registers.
            emmc::initialize(&mut unsafe { emmc::Native::new() }, emmc).map_err(|_| "eMMC init")?;
        }
        let policy = cpu::thread::Policy::capture(msr, nbio::PSP_MMIO_BASE.into())
            .map_err(|_| "CPU cache policy")?;
        let cpus = mp::start::<Self, _>(
            dies,
            microcode,
            move |msr, thread| {
                use fstart_arch::x86::cpu::msr::{APIC_BASE, ApicBase};
                let bsp = msr.reg(APIC_BASE).is_set(ApicBase::BSP);
                policy.initialize(msr, bsp, thread)
            },
            delay,
        )?;
        nbio::late::ccx_done(smn, capabilities.graphics, &mut delay, &mut notice)
            .map_err(|_| "SMU CCX done")?;
        let uma = if capabilities.graphics {
            // SAFETY: reserved, identity-mapped UMA; no display driver is active.
            Some(unsafe {
                nbio::gfx::initialize(
                    smn,
                    memory.apob.ok_or("GFX APOB")?,
                    config.graphics,
                    memory.top_mem,
                    memory.top_mem2,
                    &mut delay,
                )
            }?)
        } else {
            None
        };
        nbio::late::post_silicon(smn, identity, &mut delay).map_err(|_| "NBIO final init")?;
        ctx.fch.acpi_ports(sandstone::ACPI_PORTS);
        // SAFETY: BSP exclusively owns the decoded ACPI and SMI blocks.
        let smi = unsafe { FchSmi::new(sandstone::ACPI_PORTS, 0) };
        let tseg = smm::install(Self::NAME, &cpus.mp, &smi, memory.tseg, Self::TSEG_SIZE)?;
        let psp = psp::notify_smm(nbio::PSP_MMIO_BASE, Self::PSP, &tseg, &delay);
        let devices = config
            .emmc
            .into_iter()
            .map(|_| fstart_core::acpi::AcpiGenericDevice {
                name: fstart_core::hstr("MMC0"),
                hid: fstart_core::hstr("AMDI0040"),
                resources: [fstart_core::acpi::AcpiResource::Mmio {
                    base: 0xfedd_5000,
                    size: 0x1000,
                }]
                .into_iter()
                .collect(),
                gsiv: Some(5),
            })
            .collect();
        let aml = acpi::devices(config.i2c).map_err(|_| "ACPI devices")?;
        Ok(Silicon {
            cpus,
            psp,
            devices,
            aml,
            framebuffer: None,
            ioapics: core::array::from_fn(|i| if i == 0 { 0xfec0_1000 } else { 0 }),
            reserved_mmio: core::array::from_fn(|i| {
                if i == 0 {
                    Region::new(nbio::SMU_MMIO_BASE.into(), 0x28_0000)
                } else {
                    Region::new(0, 0)
                }
            }),
            acpi: &acpi::ACPI,
            state: uma.map(|region| (region, identity.silicon)),
        })
    }

    fn after_resources<B: Board<Soc = Self>>(_: &mut Silicon<Self>, ctx: &Ctx<'_, Self>) {
        ctx.fch.acpi_ports(sandstone::ACPI_PORTS);
    }

    fn ready_to_boot<B: Board<Soc = Self>>(
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
            let id = pci.device(address).ok_or("VGA device")?.read32(0);
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
            silicon.framebuffer = Some(
                unsafe {
                    fstart_driver_vbios::initialize(
                        pci, address, rom, rom_device, &windows, &ctx.clock,
                    )
                }
                .map_err(|e| e.name())?,
            );
        }
        B::ready_to_boot(&mut ctx.board());
        Ok(())
    }

    #[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
    fn peripherals<B: Board<Soc = Self>>(ctx: &Ctx<'_, Self>) -> noncar::Peripherals {
        B::CONFIG
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
        let gpio = ctx.board().gpio();
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

/// The early FCH setup: LPC/SPI aperture, eSPI, SPI
/// ROM, SMBus, the board's GPIOs, ROM sharing.
fn fch_init(
    fch: &Fch<NativeBus>,
    config: &FchConfig,
    clock: &mut impl Clock,
) -> Result<(), &'static str> {
    preinit::prepare_lpc(fch).map_err(|_| "LPC setup")?;
    // SAFETY: as the flow's FCH: BSP only.
    let gpio = Gpio::new(
        Fch::new(unsafe { NativeBus::new() }, fch.acpimmio()),
        Picasso::GPIO_BANK,
    );
    if let Some((espi, pads)) = &config.espi {
        gpio.configure(pads, &[]).map_err(|_| "eSPI GPIOs")?;
        // SAFETY: the SPI/eSPI BAR prepare_lpc set up; BSP only.
        let mut controller = unsafe { espi::Native::new(preinit::SPI_BASE as usize) };
        espi::initialize(&mut controller, clock, espi).map_err(|_| "eSPI link")?;
        fstart_log::info!("picasso: eSPI link up");
    }
    preinit::configure_spi(fch, config.spi).map_err(|_| "SPI setup")?;
    preinit::initialize_smbus(fch).map_err(|_| "SMBus setup")?;
    for table in config.early_gpios {
        gpio.configure(table, &[]).map_err(|_| "board GPIOs")?;
    }
    if config.disable_rom_sharing {
        legacy::disable_rom_sharing(fch.bus());
    }
    Ok(())
}
