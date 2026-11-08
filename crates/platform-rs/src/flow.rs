// SPDX-License-Identifier: MIT OR Apache-2.0
//! PSP reset through payload handoff on the shared Rs sequences of
//! `fstart_platform_amd::rs_*`. Per SoC: [`Soc`]; per board: [`Board`].
//!
//! POST codes before a stop: e1 CPU, e2 TSC, e3/e4 console path, e5/e6
//! UART, e7 APOB, e8 FCH, e9 runtime not in RAM, ea/eb fabric, ec APs,
//! ed SMU, ee/ef MPIO, f0 NBIO, f1 USB, f2 PCIe resources and I/O, f3 IOMMU
//! and requesters, f4 NBIO after training and NUMA, f5 CPU, INTx and ACPI,
//! f6 OS map, f7 SMBIOS and handoff. Progress: 10 reset, 11 console, 12
//! logging, 14 APOB, 20 fabric, 50 APs, 38 MPIO, 39 IOMMU, 3a USB, 3b PCIe,
//! 3c handoff.
use crate::{Board, Console, Soc};
use core::num::NonZeroU32;
use fstart_arch::x86::cpu::amd::msr::{self, MMIO_CFG_BASE};
use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::df::v4::Root;
use fstart_driver_amd::fch::lpc::{self, Id, IoPortDecode, Lpc};
use fstart_driver_amd::fch::{self, Acpimmio, Bus, Fch, NativeBus};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550, Ns16550Config};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// Super I/O COM1.
const COM1: u16 = 0x3f8;

fn post(code: u8) {
    // SAFETY: only the standard POST port; no config/MMIO access.
    unsafe { NativeBus::<false>::without_config() }
        .reg(fch::POST_CODE)
        .set(code);
}
fn stop(code: u8) -> ! {
    post(code);
    fstart_arch::halt()
}

pub fn run<B: Board>() -> ! {
    let name = B::Soc::NAME;
    let layout = B::Soc::LAYOUT;
    let range = |r: crate::Region| r.base..r.end().unwrap_or_else(|| stop(0xe9));
    post(0x10);
    let identity = fstart_arch::x86::mp::CpuIdentity::current();
    let signature = identity.signature;
    let package = (fstart_arch::x86::cpuid(0x8000_0001).1 >> 28) as u8;
    let bist = fstart_arch::x86::psp_reset::bsp_bist();
    if identity.vendor != fstart_arch::x86::mp::CpuVendor::Amd
        || !B::Soc::is(signature, package)
        || bist != 0
    {
        stop(0xe1);
    }
    // SAFETY: CPL0 on a checked Rs processor, BSP only.
    let msr = unsafe { NativeMsr::new() };
    let mhz = msr::tsc_mhz(&msr)
        .and_then(NonZeroU32::new)
        .unwrap_or_else(|| stop(0xe2));
    let mut delay = Tsc::new(mhz);
    // The ECAM aperture the shared ACPI publishes: 256 buses at E0000000.
    fstart_driver_amd::ccx::msr::enable_ecam(&msr, 0xe000_0000, 256);
    let config = msr.reg(MMIO_CFG_BASE).extract();
    fstart_pci::ecam::init(0xe000_0000);

    post(0x11);
    // SAFETY: the BSP owns host FCH MMIO/config; ECAM is now published.
    let bus = unsafe { NativeBus::new() };
    let fch = Fch::new(&bus, Acpimmio::DIE0);
    let uart = match B::CONSOLE {
        Console::Fch(uart) => {
            uart.prepare(&fch, &mut delay)
                .unwrap_or_else(|_| stop(0xe3));
            Ns16550Config {
                regs: AccessMode::Mmio {
                    base: uart.base(),
                    reg_shift: 2,
                    reg_width: 1,
                },
                clock_freq: 48_000_000,
                baud_rate: B::BAUD,
            }
        }
        Console::SuperIoCom1 { .. } => {
            if !bus.reg(lpc::ID).matches_all(Id::VENDOR::Amd)
                || bus.reg(lpc::IO_PORT_DECODE).get() == u32::MAX
                || bus.reg(lpc::IO_MEM_DECODE).get() == u32::MAX
            {
                stop(0xe3);
            }
            if fch.reg(fch::PM_LPC_GATING).get() == u8::MAX {
                stop(0xe4);
            }
            fch.reg(fch::PM_LPC_GATING)
                .modify(fch::LpcGating::LPC_ENABLE::SET);
            // Keep the PSP's transport setup and other decodes; do not run
            // an older generation's reset table or reconfigure an active
            // eSPI link.
            // SAFETY: BSP-exclusive LPC configuration.
            let lpc = unsafe { Lpc::new(&bus) };
            lpc.enable_sio_2e();
            lpc.enable_io_decode(IoPortDecode::SERIAL0::SET);
            // SAFETY: decoded ports, BSP ownership, board-specific SIO sequence.
            unsafe { B::early_init() };
            Ns16550Config {
                regs: AccessMode::Pio { base: COM1.into() },
                clock_freq: 1_843_200,
                baud_rate: B::BAUD,
            }
        }
    };
    let mut console_uart = Ns16550::new(uart).unwrap_or_else(|_| stop(0xe5));
    console_uart.init().unwrap_or_else(|_| stop(0xe6));
    // SAFETY: this frame and console live until the terminal halt.
    unsafe { fstart_log::init(&console_uart) };
    post(0x12);
    fstart_log::info!("{}: {} native reset", name, B::NAME);
    fstart_log::info!(
        "{}: CPUID={:#x} package={} TSC={}MHz ECAM={:#x}",
        name,
        signature,
        package,
        mhz.get(),
        config.get()
    );
    fstart_log::info!(
        "{}: reset={:#x} APOB={:#x} stack-end={:#x}",
        name,
        layout.reset.base,
        layout.apob.base,
        range(layout.stack).end
    );
    // The PM00 f7/77 row of NFX FchSongshanPei ffa53f44 (the Genoa PSP
    // leaves the same): legacy PIC/PIT, CF9, IOAPIC and HPET decode, not
    // legacy DMA/SMBus or HPET MSI.
    let decode = fch.reg(fch::PM_DECODE).get();
    // No SMM owns SMIs before `rs_smm::install`; never enter an unowned
    // SMM path. SCI delivery is independent of this mask.
    let smi_before = fch.reg(fch::SMI_TRIGGER0).get();
    if decode == u32::MAX || smi_before == u32::MAX {
        stop(0xe8);
    }
    fch.reg(fch::SMI_TRIGGER0)
        .modify(fch::SmiTrigger0::SMI_DISABLE::SET);
    fch.enable_legacy_decode();
    fch.reg(fch::PM_DECODE)
        .modify(fch::Decode::IOAPIC::SET + fch::Decode::HPET::SET);
    // The legacy SD controller (00:14.6) off, as NFX FchSongshanDxe leaves
    // it and openSIL's SdConfig default: no driver, no BAR, no INTx route.
    fch.legacy_sd_off();
    fstart_log::info!(
        "{}: FCH decode {:#x}->{:#x} SMI {:#x}->{:#x}",
        name,
        decode,
        fch.reg(fch::PM_DECODE).get(),
        smi_before,
        fch.reg(fch::SMI_TRIGGER0).get()
    );
    // OEM CPM GPIO table and connector-mode pads (AmdCpmGpioInitPeim, OEM
    // PEI): straps and selects the board wires to FCH pads, before MPIO.
    fstart_platform_amd::rs_fch::apply_cpm_table(&bus, &B::cpm_gpio());
    // SAFETY: PSP-trained, identity-mapped DRAM, reserved outside every
    // firmware writable region. The parser cannot extend this bounded view.
    let bytes = unsafe {
        core::slice::from_raw_parts(layout.apob.base as *const u8, layout.apob.size as usize)
    };
    post(0x14);
    let fstart_platform_amd::rs_discovery::Discovered {
        apob,
        topology,
        regions,
    } = fstart_platform_amd::rs_discovery::apob(bytes, crate::MAX_CPUS)
        .unwrap_or_else(|_| stop(0xe7));
    for span in [layout.early, layout.reset] {
        if !fstart_platform_amd::rs_resources::ram_covers(&regions, range(span)) {
            fstart_log::error!(
                "{}: native runtime reservation is not entirely PSP RAM",
                name
            );
            stop(0xe9)
        }
    }
    post(0x20);
    let roots = B::Soc::fabric().unwrap_or_else(|code| stop(code));
    // Rs IOD base configuration before any AP, MPIO or link code.
    if fstart_platform_amd::rs_nbio::initialize(&roots, B::NBIO).is_err() {
        stop(0xf0)
    }
    post(0x50);
    let (mp, cpus) = fstart_platform_amd::rs_mp::initialize::<{ crate::MAX_CPUS }>(
        &topology,
        fstart_platform_amd::rs_mp::Policy {
            watchdog: B::WATCHDOG,
            wc_spec: B::WC_SPEC,
        },
    )
    .unwrap_or_else(|error| {
        use fstart_arch::x86::mp::MpError;
        match error {
            MpError::PartialBringup { expected, actual } => fstart_log::error!(
                "{}: AP bring-up partial expected={} actual={}",
                name,
                expected,
                actual
            ),
            MpError::PlatformLaunchFailed(index) => {
                fstart_log::error!("{}: AP release failed at index={}", name, index)
            }
            _ => fstart_log::error!("{}: AP bring-up failed", name),
        }
        stop(0xec)
    });
    post(0x38);
    if fstart_platform_amd::rs_mpio::probe(&roots).is_err() {
        fstart_log::error!(
            "{}: retained-MPIO readiness probe failed; no training submitted",
            name
        );
        mp.park_aps();
        stop(0xee)
    }
    let smu_version = probe_smu(name, &roots, &mut delay).unwrap_or_else(|_| {
        fstart_log::error!(
            "{}: SMU test/version failed; no MPIO request submitted",
            name
        );
        mp.park_aps();
        stop(0xed)
    });
    let configs = B::mpio_ports().unwrap_or_else(|_| {
        fstart_log::error!("{}: missing/invalid MPIO board policy", name);
        mp.park_aps();
        stop(0xef)
    });
    let ports = fstart_platform_amd::rs_mpio::train(
        &roots,
        &configs,
        B::MPIO_PSP_VETTING,
        B::MPIO_ENCODING,
        range(layout.heap),
        B::release_perst,
        B::REQUIRED_PCIE,
    )
    .unwrap_or_else(|_| {
        fstart_log::error!(
            "{}: MPIO training failed; reserved DMA remains live until reset",
            name
        );
        mp.park_aps();
        stop(0xef)
    });
    fstart_log::info!("{}: PCIe trained root ports={}", name, ports.len());
    if fstart_platform_amd::rs_nbio::after_training(&roots).is_err() {
        mp.park_aps();
        stop(0xf4)
    }
    if B::USB_SMU_VERSION.is_some_and(|version| version != smu_version) {
        fstart_log::error!("{}: USB policy was measured on another SMU revision", name);
        mp.park_aps();
        stop(0xf1)
    }
    post(0x39);
    let usb_window = fstart_platform_amd::rs_resources::usb_mmio(&regions).unwrap_or_else(|| {
        fstart_log::error!("{}: no validated USB MMIO reservation", name);
        mp.park_aps();
        stop(0xf1)
    });
    let iommu_window = fstart_platform_amd::rs_resources::iommu_mmio(
        &regions,
        &usb_window,
        roots.len(),
        B::PCI_ROOT_MMIO_SIZE,
    )
    .unwrap_or_else(|| {
        fstart_log::error!("{}: no separate non-PCI IOMMU reservation", name);
        mp.park_aps();
        stop(0xf3)
    });
    // SAFETY: checked live single-socket roots, complete disjoint PSP MMIO
    // reservation, BSP-exclusive fabric; no DMA controller has started yet.
    let mut iommus =
        unsafe { fstart_platform_amd::rs_iommu::initialize(&roots, iommu_window.clone(), 0x149e) }
            .unwrap_or_else(|_| {
                fstart_log::error!("{}: IOMMU mapping/capture failed; no retry/handoff", name);
                mp.park_aps();
                stop(0xf3)
            });
    // SAFETY: as above; translation stays off, USB has not started.
    unsafe { fstart_platform_amd::rs_iommu::initialize_policy(&mut iommus, signature) }
        .unwrap_or_else(|_| {
            fstart_log::error!("{}: IOMMU policy failed; no retry/handoff", name);
            mp.park_aps();
            stop(0xf3)
        });
    // The PSP window: SMM's C2P mailbox and the PSP's EINJ service. Without
    // it the platform only loses those, not the boot.
    let psp = fstart_platform_amd::rs_resources::psp_mmio(&regions, &iommu_window)
        // SAFETY: BSP-exclusive DF4/SMN; the slot after the IOMMU slots is
        // reserved non-PCI MMIO, identity mapped uncached.
        .and_then(|window| unsafe { fstart_platform_amd::rs_psp::map(&roots, window) }.ok());
    if psp.is_none() {
        fstart_log::warn!("{}: no PSP window; SMM and EINJ unavailable", name);
    }
    // The NBIO IOAPICs before any controller masters, as AGESA programs them
    // in PEI long before an xHCI driver runs; every redirection is masked.
    let cpu =
        fstart_platform_amd::rs_cpu::snapshot::<{ crate::MAX_CPUS }>(&mp).unwrap_or_else(|_| {
            fstart_log::error!("{}: ACPI CPU audit failed; no handoff", name);
            mp.park_aps();
            stop(0xf5)
        });
    // SAFETY: IOMMU policy proved the global SMN relocation; BSP-exclusive
    // DF/config/SMN, IF=0 on all CPUs, SMI disabled, no INTx consumer yet.
    let interrupts =
        unsafe { fstart_platform_amd::rs_ioapic::initialize(&roots, iommu_window, &cpu.ids) }
            .unwrap_or_else(|_| {
                fstart_log::error!("{}: NBIO INTx setup failed; no retry/handoff", name);
                mp.park_aps();
                stop(0xf5)
            });
    post(0x3a);
    let usb_policy = B::usb_policy().unwrap_or_else(|| {
        fstart_log::error!("{}: missing USB board policy", name);
        mp.park_aps();
        stop(0xf1)
    });
    // The payload's xHCI driver starts the controllers.
    fstart_platform_amd::rs_usb::initialize(
        &roots,
        usb_window.clone(),
        &usb_policy,
        fstart_driver_amd::fch::songshan::usb::Interface::Relative,
        B::Soc::SMU_ID_LIMIT,
    )
    .unwrap_or_else(|_| {
        fstart_log::error!(
            "{}: USB setup failed; stopping before payload handoff",
            name
        );
        mp.park_aps();
        stop(0xf1)
    });
    post(0x3b);
    let pci_window = fstart_platform_amd::rs_resources::pcie_mmio(
        &regions,
        &usb_window,
        roots.len(),
        B::PCI_ROOT_MMIO_SIZE,
    )
    .unwrap_or_else(|| {
        fstart_log::error!("{}: no disjoint bounded PCIe MMIO pool", name);
        mp.park_aps();
        stop(0xf2)
    });
    let pci64 = fstart_platform_amd::rs_resources::pcie_mmio64(
        &regions,
        roots.len(),
        B::PCI_ROOT_MMIO64_SIZE,
        fstart_platform_amd::rs_resources::physical_address_bits(),
    )
    .unwrap_or_else(|| {
        fstart_log::error!("{}: no above-4-GiB PCIe pool", name);
        mp.park_aps();
        stop(0xf2)
    });
    fstart_log::info!(
        "{}: PCIe windows={:#x}..{:#x} 64-bit={:#x}..{:#x}",
        name,
        pci_window.start,
        pci_window.end,
        pci64.start,
        pci64.end
    );
    // The ACPI blocks at the vendor's PM base; the primary root keeps them
    // when the I/O space is partitioned.
    fstart_platform_amd::rs_fch::acpi_decode(&fch, B::Soc::PMBASE).unwrap_or_else(|_| {
        mp.park_aps();
        stop(0xf2)
    });
    if !fch
        .reg(fch::SMI_TRIGGER0)
        .is_set(fch::SmiTrigger0::SMI_DISABLE)
    {
        fstart_log::error!("{}: refusing I/O routing with SMI generation enabled", name);
        mp.park_aps();
        stop(0xf2)
    }
    // SAFETY: checked one-socket Rs SoC, all CPUs run with IF=0, SMI
    // generation is disabled and the single BSP executes MMIO-only DF
    // configuration until the complete I/O partition has read back.
    let io_budgets =
        unsafe { fstart_platform_amd::rs_io::initialize(&roots, &fch, B::Soc::PMBASE) }
            .unwrap_or_else(|_| {
                fstart_log::error!("{}: I/O routing failed; no PIO/retry/handoff", name);
                mp.park_aps();
                stop(0xf2)
            });
    // SMM, only for the PSP: SmmInfo and its ACPI EINJ service. After the
    // I/O partition, which needs SMIs off and puts the SMI command port on
    // the FCH's root. TSEG is withheld from the OS either way.
    let tseg = fstart_platform_amd::rs_smm::place_tseg(
        &regions,
        1 << 32,
        &[
            range(layout.cache),
            range(layout.apob),
            range(layout.payload_heap),
        ],
    );
    let einj = psp.zip(tseg.clone()).and_then(|(psp, tseg)| {
        let config = fstart_platform_amd::rs_smm::Config {
            psp_bar: psp.bar(),
            tsc_mhz: mhz.get(),
        };
        // SAFETY: BSP with the APs parked under `mp`; TSEG is RAM outside
        // every firmware reservation, withheld from the OS; the PSP BAR is
        // mapped below 4 GiB.
        let installed = unsafe { fstart_platform_amd::rs_smm::install(&mp, tseg, config) }.ok()?;
        fstart_log::info!(
            "{}: SMM TSEG={:#x} mask={:#x} locked",
            name,
            installed.base,
            installed.mask
        );
        psp.einj(&regions, &delay).ok()
    });
    if einj.is_none() {
        fstart_log::warn!("{}: no PSP EINJ service", name);
    }
    let mut pci = fstart_platform_amd::rs_pci::initialize(
        &roots,
        &ports,
        pci_window,
        Some(pci64),
        &io_budgets,
        &usb_window,
        B::RETAINED_PCIE,
    )
    .unwrap_or_else(|_| {
        fstart_log::error!("{}: PCIe resource ownership failed; no retry/handoff", name);
        mp.park_aps();
        stop(0xf2)
    });
    if let Some(identity) = B::PRIMARY_VGA {
        // SAFETY: checked allocated display path, BSP-exclusive DF/config;
        // the legacy VGA aperture is never firmware storage or OS RAM.
        unsafe { fstart_platform_amd::rs_pci::route_vga(&roots, &mut pci, identity) }
            .unwrap_or_else(|_| {
                fstart_log::error!("{}: primary VGA routing failed; no retry/handoff", name);
                mp.park_aps();
                stop(0xf2)
            });
    }
    // SAFETY: checked roots, owned stable ports with DMA off, IOMMU policy
    // installed, BSP-exclusive SMN and all CPUs still have IF=0.
    unsafe { fstart_platform_amd::rs_pci::initialize_requesters(&roots, &pci) }.unwrap_or_else(
        |_| {
            fstart_log::error!("{}: PCIe requester identity failed; no retry/handoff", name);
            mp.park_aps();
            stop(0xf3)
        },
    );
    // An FCH UART's IRQ 4 comes from its FCH source; a Super I/O's arrives
    // over LPC SERIRQ.
    let (console, route) = match B::CONSOLE {
        Console::Fch(uart) => (
            fstart_platform_amd::rs_acpi::Console::Mmio {
                base: uart.base(),
                irq: 4,
            },
            Some((uart.intr_source(), 4)),
        ),
        Console::SuperIoCom1 { irq } => (
            fstart_platform_amd::rs_acpi::Console::Io { port: COM1, irq },
            None,
        ),
    };
    fstart_platform_amd::rs_fch::prepare_acpi_mode(&fch, route).unwrap_or_else(|_| {
        fstart_log::error!("{}: direct ACPI mode failed; no handoff", name);
        mp.park_aps();
        stop(0xf5)
    });
    let numa = fstart_platform_amd::rs_numa::discover(&apob, &regions, &cpus, B::MEMORY_CHANNELS)
        .unwrap_or_else(|_| {
            fstart_log::error!("{}: NUMA domains unavailable; no handoff", name);
            mp.park_aps();
            stop(0xf4)
        });
    let rsdp = fstart_platform_amd::rs_acpi::install(fstart_platform_amd::rs_acpi::Runtime {
        roots: &roots,
        iommus: &iommus,
        cpu: &cpu,
        interrupts: &interrupts,
        pci: &pci,
        console: Some(console),
        hpet_id: bus.reg(fch::Reg::<u32>::mmio(fch::HPET)).get(),
        pmbase: B::Soc::PMBASE,
        iapc_boot_arch: if B::LEGACY_DEVICES {
            fstart_acpi::platform::x86::IAPC_LEGACY_DEVICES
        } else {
            0
        },
        numa: Some(&numa),
        psp: psp.map(|p| p.bar()),
        einj,
    })
    .unwrap_or_else(|_| {
        fstart_log::error!("{}: native ACPI construction failed; no handoff", name);
        mp.park_aps();
        stop(0xf5)
    });
    fstart_log::info!("{}: ACPI RSDP={:#x}", name, rsdp);
    #[cfg(feature = "payload-linux")]
    let os_map = fstart_platform_amd::rs_resources::os_memory_map(
        &regions,
        &[
            range(layout.cache),
            range(layout.apob),
            range(layout.linux_params),
        ]
        .into_iter()
        .chain(tseg.clone())
        .collect::<alloc::vec::Vec<_>>(),
    )
    .unwrap_or_else(|_| {
        fstart_log::error!("{}: final OS memory map rejected", name);
        mp.park_aps();
        stop(0xf6)
    });
    mp.park_aps();
    post(0x3c);
    // SAFETY: APs are parked, IF/SMI remain off. The common fixed-MTRR DRAM
    // routing is checked before publishing in reserved F-segment RAM.
    unsafe {
        fstart_platform_amd::rs_smbios::publish(
            &fstart_platform_amd::rs_smbios::Board {
                manufacturer: B::SMBIOS_MANUFACTURER,
                product: B::SMBIOS_PRODUCT,
                socket: B::Soc::SOCKET,
                rom_size: B::FLASH_SIZE,
                max_dimm_mib: B::SMBIOS_MAX_DIMM_MIB,
                channels: B::MEMORY_CHANNELS,
            },
            &apob,
            &topology,
            &regions,
            &cpu.ids,
            range(layout.heap),
        )
    }
    .unwrap_or_else(|error| {
        fstart_log::error!(
            "{}: SMBIOS publication failed {:?}; no handoff",
            name,
            error
        );
        stop(0xf7)
    });
    #[cfg(feature = "payload-linux")]
    {
        fstart_platform_amd::rs_linux::launch(
            &os_map,
            rsdp,
            range(layout.reset),
            layout.linux_params.base,
            crate::PDPT_COUNT << 39,
        )
    }
    #[cfg(feature = "payload-uefi")]
    {
        fstart_platform_amd::rs_pci::enable_bridge_mastering(&pci).unwrap_or_else(|_| {
            fstart_log::error!("{}: bridge bus mastering failed; no handoff", name);
            stop(0xf7)
        });
        crate::boot::launch::<B::Soc>(&console_uart, &regions, &roots, rsdp, tseg)
    }
    #[cfg(not(any(feature = "payload-linux", feature = "payload-uefi")))]
    {
        let _ = (pci, console_uart, regions, roots, rsdp, tseg);
        fstart_log::info!("{}: no payload; halting", name);
        fstart_log::flush();
        fstart_arch::halt()
    }
}

/// The SMU answers its test message and reports its firmware revision.
fn probe_smu(name: &str, roots: &[Root], delay: &mut Tsc) -> Result<u32, ()> {
    use fstart_driver_amd::{
        df::v4,
        smn::Smn,
        smu::{self, v13},
    };
    use fstart_pci::ecam::GlobalEcam;
    let primary = roots
        .iter()
        .find(|r| r.ios.physical_index == v4::PRIMARY_IOS)
        .ok_or(())?;
    // SAFETY: MPIO paths are checked/idle, BSP owns SMN; AP callbacks never
    // access these mailboxes. SmuDefs.h: Test=1, GetSmuVersion=2.
    let smn = unsafe { Smn::on_bus(GlobalEcam, primary.route.base) };
    let echo = v13::MAILBOX
        .request(&smn, smu::TEST, [0xaa55_aa55, 0, 0, 0, 0, 0], delay)
        .map_err(|_| ())?;
    let version = v13::MAILBOX
        .request(&smn, 2, [0; 6], delay)
        .map_err(|_| ())?;
    fstart_log::info!(
        "{}: SMU test={:#x} version={:#x}",
        name,
        echo[0],
        version[0]
    );
    if echo[0] == 0 || version[0] == 0 || version[0] == u32::MAX {
        return Err(());
    }
    Ok(version[0])
}
