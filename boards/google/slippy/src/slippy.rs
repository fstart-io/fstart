// SPDX-License-Identifier: MIT OR Apache-2.0
//! Slippy baseboard wiring and event policy (coreboot devicetree.cb and ec.h).
use fstart_driver_chromeec::{
    Ec, Error, EventMasks,
    HostEvent::*,
    NativeIo,
    handoff::{self, EventPolicy},
};
use fstart_platform_intel::haswell::{
    APM_CNT, Config, Gpios, IrqRoute, LpcGenericIoDecode, PadTable, SerialIoMask,
};

pub(super) const fn config(pads: &'static PadTable) -> Config<true> {
    let mut c = Config::new(Gpios::Lp(pads));
    c.northbridge.memory.double_refresh = true;
    c.northbridge.igd.panel.hotplug = [6, 0, 6];
    c.southbridge.lpc_generic_io = &[
        LpcGenericIoDecode {
            base: 0x800,
            size: 0x100,
        },
        LpcGenericIoDecode {
            base: 0x900,
            size: 0x100,
        },
    ];
    c.southbridge.irq = IRQ;
    c.southbridge.ec_present = true;
    c.southbridge.sealed_case = true;
    c.southbridge.alt_gp_smi_en = EC_SMI_GPI_MASK;
    c.southbridge.gpe0_en = [0, 0x10, 0, 0];
    c.southbridge.pcie_force_aspm = 1;
    // SDMA, I2C0/1, 3.3 V, ACPI mode.
    c.southbridge.serialio_enable = SerialIoMask::SDMA | SerialIoMask::I2C0 | SerialIoMask::I2C1;
    c.southbridge.sio_acpi_mode = true;
    c
}

const IRQ: &[IrqRoute] = {
    // DxxIP nibbles in function order; DxxIR nibbles in INT[A..D] order.
    const fn route(device: u8, pins: Option<u32>, pirqs: Option<u16>) -> IrqRoute {
        IrqRoute {
            device,
            pins,
            pirqs,
        }
    }
    &[
        route(31, Some(0x0300_2100), Some(0x0126)),
        route(29, Some(1), Some(0x3333)),
        route(28, Some(0x2301), Some(0x3210)),
        route(27, Some(1), Some(0x6666)),
        route(26, Some(1), None),
        route(22, Some(0), Some(0)),
        route(21, None, Some(0x5554)),
        route(20, Some(1), Some(0x2222)),
        route(23, None, Some(0x7777)),
    ]
};

/// `ALT_GP_SMI_EN`/`ALT_GP_SMI_STS` bit for Slippy's EC SMI pin (GPIO34).
pub(crate) const EC_SMI_GPI_MASK: u32 = 1 << 2;

/// Slippy ChromeEC host-event masks and S4/S5 USB charging ports.
pub(crate) const EC_POLICY: EventPolicy = EventPolicy {
    sci: LidClosed.mask()
        | LidOpen.mask()
        | AcConnected.mask()
        | AcDisconnected.mask()
        | BatteryLow.mask()
        | BatteryCritical.mask()
        | Battery.mask()
        | ThermalThreshold.mask(),
    smi: LidClosed.mask(),
    wake_s5: LidOpen.mask() | PowerButton.mask(),
    wake_s3: LidOpen.mask() | PowerButton.mask() | KeyPressed.mask(),
    usb_charge_ports: 2,
};

/// AP POST checkpoints for the EC's port-80 history.
pub fn post(code: u8) {
    // SAFETY: Slippy's LPC bridge routes the POST byte to the EC.
    unsafe { fstart_core::pio::outb(0x80, code) };
}

pub(super) fn ec() -> Result<Ec<NativeIo>, Error> {
    // SAFETY: BSP pre-OS or serialized SMM at an EC ownership handoff.
    Ec::discover(unsafe { NativeIo::new() })
}

/// BSP result for the permanent handler. Settled masks are logged separately
/// so a failure of a later setter doesn't erase which route actually changed.
pub fn init_ec(resume: bool) -> Option<EventMasks> {
    post(0xf0);
    let result = ec().and_then(|mut ec| {
        handoff::init_events(
            &mut ec,
            &EC_POLICY,
            resume,
            &fstart_arch::x86::Udelay,
            |op, res| match res {
                Ok(()) => fstart_log::debug!("Slippy: EC {} set", op),
                Err(e) => fstart_log::warn!(
                    "Slippy: EC {}: {} ({})",
                    op,
                    e.description(),
                    e.result_code()
                ),
            },
        )
    });
    post(0xf1);
    match result {
        Ok(m) => Some(m),
        Err(e) => {
            fstart_log::warn!(
                "Slippy: EC routing unavailable: {} ({}); skipping EC sleep/SMI policy",
                e.description(),
                e.result_code()
            );
            None
        }
    }
}

pub fn handoff_ec(policy: Option<EventMasks>) {
    let cmd = handoff::apm_handoff_cmd(policy);
    // SAFETY: called once by ramstage after start_cpus installs the handler.
    unsafe { fstart_core::pio::outb(APM_CNT, cmd) }
}
