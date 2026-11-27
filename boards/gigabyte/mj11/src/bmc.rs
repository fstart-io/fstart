// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AST2500 BMC: LPC/Super I/O setup for COM1, readiness wait, VGA
//! subsystem ID, IPMI KCS end-of-POST calls, and DIMM SMBus handoff.
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::{
    FchHw,
    acpimmio::iomux,
    early::{self, PowerFailure},
    gpio::{Control, Gpio},
    lpc::{self, IoPortDecode},
};
use fstart_driver_amd::window::Window;
use fstart_driver_ipmi::kcs::{self, Kcs};
use fstart_driver_ipmi::{RestorePolicy, SelRecord};
use fstart_driver_superio::aspeed_ast2400::{self as ast, Ast2400, SuperIoConfig};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_pci::ecam::GlobalEcam;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use fstart_platform_amd::zeppelin::Die;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
use zerocopy::little_endian::{U16, U32};

pub const COM1: u16 = 0x3f8;
pub const COM1_IRQ: u8 = 4;

/// COM1, routed to the AST2500's SUART1.
pub const CONSOLE: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio { base: COM1 as u64 },
    clock_freq: 1_843_200,
    baud_rate: 115_200,
};

register_bitfields![u32,
    /// The IOMUX bytes of pins 136-139 as one dword.
    Iomux136 [
        PIN137_BIT1 OFFSET(9) NUMBITS(1) [],
        PIN139_HIGH OFFSET(28) NUMBITS(4) []
    ]
];

/// The select of the DIMM SMBus switch: high connects the DIMMs (SPD and
/// thermal sensors, behind the PCA954x at 0x71) to the FCH's SMBus port 0,
/// low to the BMC.
const GPIO137: u8 = 137;

/// Gigabyte OEM NetFn; the request starts with their IANA 0x003c0a.
const NETFN_GIGABYTE: u8 = 0x2e;
const SET_ERP: u8 = 0x20;
/// ErP off, the Setup default.
const ERP_OFF: [u8; 5] = [0x0a, 0x3c, 0x00, 0x0d, 0x1c];

/// LPC decodes and the AST2500 SuperIO routing for COM1, before the console.
pub fn early_init(fch: &impl FchHw) {
    lpc::enable_super_io(fch, true, false);
    lpc::enable_io_decode(fch, IoPortDecode::SERIAL0::SET);
    // AST2500 SIO runtime registers (0xa00-0xbff) and the IPMI KCS interface.
    let _ = lpc::set_wideio_range(fch, 0xa00, 512);
    let _ = lpc::set_wideio_range(fch, 0xca0, 16);
    let Ok(sio) = Ast2400::new_at_base(SuperIoConfig::default(), ast::CONFIG_PORT) else {
        return;
    };
    sio.route_uart1_pins();
    // SUART4 carries the BMC's IPMI SOL; the host must leave it alone.
    sio.disable(&[
        ast::SUART2,
        ast::SWC,
        ast::KBC,
        ast::GPIO,
        ast::SUART3,
        ast::SUART4,
        ast::MAILBOX,
    ]);
    sio.enable_uart(ast::SUART1, COM1, COM1_IRQ, 0);
}

/// Wait for the BMC before its VGA is used.
pub fn wait_ready(gpio: &Gpio<impl Window>, clock: Tsc) {
    crate::gpio::wait_for_bmc(gpio, &mut { clock });
}

/// GbtPreBootSvid: the AST2500 VGA on the last bus behind 00:01.1 gets the
/// Gigabyte subsystem ID; config register 0x80 unlocks the write.
pub fn set_vga_ssid(die0: &Die<GlobalEcam>) {
    let pci = die0.pci();
    // SAFETY: the enumerated root port and the device behind it.
    unsafe {
        let bus = (pci.read(PciAddress::new(0, 0, 1, 1), 0x18) >> 16) as u8;
        let vga = PciAddress::new(0, bus, 0, 0);
        if bus == 0 || pci.read(vga, 0) != 0x2000_1a03 {
            return;
        }
        pci.write(vga, 0x80, 0xaa1a_03aa);
        pci.write(vga, 0x2c, crate::CONFIG.subsystem_id);
        pci.write(vga, 0x80, 0);
    }
}

/// The vendor's SMBusSwitch at ReadyToBoot: hand the DIMM SMBus to the BMC,
/// which reads the DIMM temperature (`DIMMG0_TEMP`) from it while the OS
/// runs. The host keeps the bus only during POST, when the PSP reads the
/// SPDs. Linux still probes SMBus port 0 for SPDs at boot because SMBIOS
/// lists DDR4 DIMMs, so `ee1004 ... error -5` is expected on this board.
/// The vendor writes the IOMUX of pins 136-139 through SMN; it is the same
/// register as the ACPI MMIO one.
pub fn hand_dimm_smbus_to_bmc(gpio: &Gpio<impl Window>, fch: &impl FchHw) {
    // Bit 1 of the GPIO137 IOMUX byte set, bits 7:4 of pin 139 cleared.
    fch.acpi()
        .at(iomux::at::<u32, Iomux136::Register>(136))
        .modify(Iomux136::PIN137_BIT1::SET + Iomux136::PIN139_HIGH.val(0));
    // GPIO137 driven low, keeping the drive strength and STS_EN bits.
    if let Ok(control) = gpio.control(GPIO137) {
        let old = control.extract();
        control.write(
            Control::DRIVE_STRENGTH.val(old.read(Control::DRIVE_STRENGTH))
                + Control::STATUS_ENABLE.val(old.read(Control::STATUS_ENABLE))
                + Control::OUTPUT_ENABLE::SET,
        );
    }
    crate::clear_pm1(fch);
    // Pins 113-117 back to function 0 (SMBus0): two unaligned dword
    // writes, as in the vendor code.
    fch.acpi().at(iomux::at::<u32, ()>(113)).set(0);
    fch.acpi().at(iomux::at::<u32, ()>(114)).set(0);
}

pub fn post_done(fch: &impl FchHw, clock: Tsc) {
    // SAFETY: the KCS ports the board decodes in early_init; only used here.
    let mut kcs = unsafe { Kcs::new(kcs::DEFAULT_PORT, kcs::DEFAULT_SPACING, clock) };

    // GbtSystemBootEvent: SEL "System Boot Initiated" (system event sensor).
    let boot = SelRecord {
        record_id: U16::new(0),
        record_type: 2,
        timestamp: U32::new(0),
        generator_id: U16::new(1),
        event_message_revision: 4,
        sensor_type: 0x12,
        sensor_number: 0,
        event_dir_type: 0x6f,
        event_data: [0x01, 0xff, 0xff],
    };
    if kcs.add_sel(&boot).is_err() {
        fstart_log::warn!("Adding the boot SEL entry failed");
    }

    // GbtACLossIPMI: the BMC's power restore policy replaces the AC-loss
    // setting.
    match kcs.chassis_status() {
        Ok(status) => {
            let policy = status.restore_policy();
            let state = match policy {
                RestorePolicy::Off => Some(PowerFailure::Off),
                RestorePolicy::Previous => Some(PowerFailure::Previous),
                RestorePolicy::On => Some(PowerFailure::On),
                RestorePolicy::Unknown => None,
            };
            if let Some(state) = state {
                early::set_power_failure_state(fch, state);
                fstart_log::info!("BMC power restore policy {}", policy as u8);
            }
        }
        Err(_) => fstart_log::warn!("Get Chassis Status failed"),
    }

    let mut buffer = [0; 8];
    if kcs
        .request(NETFN_GIGABYTE, SET_ERP, &ERP_OFF, &mut buffer)
        .is_err()
    {
        fstart_log::warn!("BMC ErP command failed");
    }
}
