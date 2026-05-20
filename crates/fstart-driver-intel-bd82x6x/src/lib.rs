//! Intel bd82x6x / 6 Series / C200 "Cougar Point" PCH driver.
//!
//! This is the Sandy Bridge-era PCH used by the ThinkPad X220.  The structure
//! mirrors the existing ICH8/ICH7 fstart drivers while keeping register names
//! and comments close to coreboot `src/southbridge/intel/bd82x6x/` for review.
//! The first port covers pre-console LPC/GPIO/SMBus setup and records the
//! board policy needed for later SATA/USB/HDA/ME/SMM finalization.

#![no_std]

use fstart_ecam as ecam;
use fstart_gpio_ich::IchGpio;
use fstart_mmio::MmioReadWrite;
use fstart_pci::{pci_type0_config, PCI_COMMAND_BITS};
use fstart_pmio_ich as pmio;
use fstart_pmio_ich::PmIo;

pub use fstart_gpio_ich::{GpioConfig, GpioDir, GpioLevel, GpioMode, GpioPin, GpioReset};
pub use fstart_hda::{HdaConfig, HdaVerbTable, PinConfig};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{
    EarlyInit, FinalizeInit, PostDramInit, PreConsoleInit, ServiceError, SmBus, Southbridge,
};
use fstart_smbus_intel::I801SmBus;
use serde::{Deserialize, Serialize};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

mod pch {
    pub const LPC_DEV: u8 = 31;
    pub const LPC_FUNC: u8 = 0;
    pub const SMBUS_DEV: u8 = 31;
    pub const SMBUS_FUNC: u8 = 3;

    pub const LPC_EN_ALL: u16 =
        (1 << 13) | (1 << 12) | (1 << 11) | (1 << 10) | (1 << 3) | (1 << 2) | (1 << 1) | (1 << 0);
    pub const PCH_DISABLE_ALWAYS: u32 = (1 << 0) | (1 << 26);
    pub const GPE0_EN: u16 = 0x28;
    pub const ALT_GP_SMI_EN: u16 = 0x38;

    pub const DEFAULT_PMBASE: u16 = 0x0500;
    pub const DEFAULT_GPIOBASE: u16 = 0x0480;
    pub const SMBUS_SLAVE_ADDR: u8 = 0x24;
}

register_bitfields! [u32,
    /// Root Complex miscellaneous control.
    RC_REG [
        EXTENDED_CMOS_ENABLE OFFSET(2) NUMBITS(1) []
    ],
    /// General Control and Status.
    GCS_REG [
        NO_REBOOT OFFSET(5) NUMBITS(1) []
    ],
    /// Flash descriptor observer data.
    FDOD_REG [
        COMPONENT_FREQ OFFSET(24) NUMBITS(3) []
    ],
    /// General PM config 3 / reset-control bits.
    ETR3_REG [
        CWORWRE OFFSET(18) NUMBITS(1) [],
        CF9GR OFFSET(20) NUMBITS(1) []
    ]
];

register_bitfields! [u16,
    /// SMBus D31:F3 config 0x80 clock-gating controls.
    SMBUS_CFG80_REG [
        BIT8 OFFSET(8) NUMBITS(1) [],
        BIT10 OFFSET(10) NUMBITS(1) [],
        BIT12 OFFSET(12) NUMBITS(1) [],
        BIT14 OFFSET(14) NUMBITS(1) []
    ]
];

register_bitfields! [u8,
    /// LPC ACPI control register.
    ACPI_CNTL_REG [
        ACPI_EN OFFSET(7) NUMBITS(1) []
    ],
    /// LPC GPIO control register.
    GPIO_CNTL_REG [
        GPIO_EN OFFSET(4) NUMBITS(1) []
    ],
    /// LPC SPI prefetch/cache control.
    BIOS_CNTL_REG [
        PREFETCH_CACHE_CTRL OFFSET(2) NUMBITS(2) []
    ],
    /// Flash software sequence frequency control.
    SSFC_REG [
        COMPONENT_FREQ OFFSET(0) NUMBITS(3) []
    ],
    /// Boot BIOS update control.
    BUC_REG [
        DISABLE_GBE OFFSET(5) NUMBITS(1) []
    ]
];

pci_type0_config! {
    /// bd82x6x LPC bridge PCI configuration space.
    pub struct Bd82x6xLpcPciConfig {
        (0x40 => pub pmbase: MmioReadWrite<u32>),
        (0x44 => pub acpi_cntl: MmioReadWrite<u8, ACPI_CNTL_REG::Register>),
        (0x45 => _reserved_lpc0),
        (0x48 => pub gpio_base: MmioReadWrite<u32>),
        (0x4c => pub gpio_cntl: MmioReadWrite<u8, GPIO_CNTL_REG::Register>),
        (0x4d => _reserved_lpc1),
        (0x60 => pub pirq_rout: [MmioReadWrite<u8>; 4]),
        (0x64 => pub serirq_cntl: MmioReadWrite<u8>),
        (0x65 => _reserved_lpc2),
        (0x68 => pub pirqe_rout: [MmioReadWrite<u8>; 4]),
        (0x6c => _reserved_lpc3),
        (0x80 => pub lpc_io_dec: MmioReadWrite<u16>),
        (0x82 => pub lpc_en: MmioReadWrite<u16>),
        (0x84 => pub gen_dec: [MmioReadWrite<u32>; 4]),
        (0x94 => _reserved_lpc4),
        (0xac => pub etr3: MmioReadWrite<u32, ETR3_REG::Register>),
        (0xb0 => _reserved_lpc5),
        (0xb8 => pub gpio_rout: MmioReadWrite<u32>),
        (0xbc => _reserved_lpc6),
        (0xdc => pub bios_cntl: MmioReadWrite<u8, BIOS_CNTL_REG::Register>),
        (0xdd => _reserved_lpc7),
        (0xf0 => pub rcba: MmioReadWrite<u32>),
        (0xf4 => _reserved_lpc8),
        (0x1000 => @END),
    }
}

pci_type0_config! {
    /// bd82x6x SMBus PCI configuration space.
    pub struct Bd82x6xSmbusPciConfig {
        (0x40 => _reserved_smb0),
        (0x80 => pub cfg80: MmioReadWrite<u16, SMBUS_CFG80_REG::Register>),
        (0x82 => _reserved_smb1),
        (0x1000 => @END),
    }
}

register_structs! {
    /// bd82x6x Root Complex Register Block (RCRB/RCBA) sparse overlay.
    pub RcbaRegs {
        (0x0000 => _reserved0),
        (0x3100 => pub d31ip: MmioReadWrite<u32>),
        (0x3104 => _reserved_d30ip),
        (0x3108 => pub d29ip: MmioReadWrite<u32>),
        (0x310c => pub d28ip: MmioReadWrite<u32>),
        (0x3110 => pub d27ip: MmioReadWrite<u32>),
        (0x3114 => pub d26ip: MmioReadWrite<u32>),
        (0x3118 => pub d25ip: MmioReadWrite<u32>),
        (0x311c => _reserved_ip),
        (0x3124 => pub d22ip: MmioReadWrite<u32>),
        (0x3128 => pub d20ip: MmioReadWrite<u32>),
        (0x312c => _reserved_ir0),
        (0x3140 => pub d31ir: MmioReadWrite<u16>),
        (0x3142 => _reserved_d30ir),
        (0x3144 => pub d29ir: MmioReadWrite<u16>),
        (0x3146 => pub d28ir: MmioReadWrite<u16>),
        (0x3148 => pub d27ir: MmioReadWrite<u16>),
        (0x314a => _reserved_ir1),
        (0x314c => pub d26ir: MmioReadWrite<u16>),
        (0x314e => _reserved_ir2),
        (0x3150 => pub d25ir: MmioReadWrite<u16>),
        (0x3152 => _reserved_ir3),
        (0x315c => pub d22ir: MmioReadWrite<u16>),
        (0x315e => _reserved_ir4),
        (0x3160 => pub d20ir: MmioReadWrite<u16>),
        (0x3162 => _reserved_oic),
        (0x31fe => pub oic: MmioReadWrite<u16>),
        (0x3200 => _reserved_rc),
        (0x3400 => pub rc: MmioReadWrite<u32, RC_REG::Register>),
        (0x3404 => _reserved_gcs),
        (0x3410 => pub gcs: MmioReadWrite<u32, GCS_REG::Register>),
        (0x3414 => pub buc: MmioReadWrite<u8, BUC_REG::Register>),
        (0x3415 => _reserved_fd),
        (0x3418 => pub fd: MmioReadWrite<u32>),
        (0x341c => _reserved_spi),
        (0x3893 => pub ssfc: MmioReadWrite<u8, SSFC_REG::Register>),
        (0x3894 => _reserved_fdoc),
        (0x38b0 => pub fdoc: MmioReadWrite<u32>),
        (0x38b4 => pub fdod: MmioReadWrite<u32, FDOD_REG::Register>),
        (0x38b8 => _reserved_vscc),
        (0x38c4 => pub lvscc: MmioReadWrite<u32>),
        (0x38c8 => pub uvscc: MmioReadWrite<u32>),
        (0x38cc => @END),
    }
}

#[derive(Clone, Copy)]
struct Rcba {
    base: usize,
}

impl Rcba {
    const fn new(base: usize) -> Self {
        Self { base }
    }

    fn regs(&self) -> &'static RcbaRegs {
        // SAFETY: RCBA is programmed and enabled before callers touch RCRB.
        unsafe { &*(self.base as *const RcbaRegs) }
    }
}

/// Fixed LPC decode selector for common legacy devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LpcFixedDecode {
    /// COM1 0x3f8 / COM2 0x2f8, used by dock/superio serial paths.
    #[default]
    Com1Com2,
}

/// LPC decode policy for Cougar Point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bd82x6xLpcDecodeConfig {
    /// Fixed legacy decode selection.  X220 has no always-on SuperIO UART, but
    /// keeping COM decode open matches the fstart x86 console patterns.
    #[serde(default)]
    pub fixed: LpcFixedDecode,
    /// Raw GEN_DEC values copied from coreboot devicetree (`gen1_dec`, etc.).
    /// X220 uses 0x7c1601, 0x0c15e1, and 0x0c06a1 for EC/PMH/TPM/legacy I/O.
    #[serde(default)]
    pub gen_dec: [u32; 4],
}

impl Default for Bd82x6xLpcDecodeConfig {
    fn default() -> Self {
        Self {
            fixed: LpcFixedDecode::Com1Com2,
            gen_dec: [0; 4],
        }
    }
}

/// Late RCBA interrupt/route policy.  Values remain raw so board RON can be
/// compared with coreboot `early_rcba.c`, `devicetree.cb`, and ACPI IRQ ASL.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Bd82x6xLateRcbaConfig {
    #[serde(default)]
    pub d31ip: u32,
    #[serde(default)]
    pub d29ip: u32,
    #[serde(default)]
    pub d28ip: u32,
    #[serde(default)]
    pub d27ip: u32,
    #[serde(default)]
    pub d26ip: u32,
    #[serde(default)]
    pub d25ip: u32,
}

/// Cougar Point PCH configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelBd82x6xConfig {
    /// Root Complex Base Address.  Coreboot X220 uses 0xfed1c000.
    pub rcba: u64,
    /// ECAM base address supplied by the Sandy Bridge host bridge.
    #[serde(default = "default_ecam_base")]
    pub ecam_base: u64,
    /// PIRQ routing (A..H).
    pub pirq_routing: [u8; 8],
    /// GPE0 enable bits.
    #[serde(default)]
    pub gpe0_en: u32,
    /// GPI routing selectors for GPIO0..15: 0=none, 1=SMI, 2=SCI.  Coreboot
    /// X220 routes GPI1 and GPI13 to SCI.
    #[serde(default)]
    pub gpi_routing: [u8; 16],
    /// Alternate GPI SMI enable bits.
    #[serde(default)]
    pub alt_gp_smi_en: u16,
    /// LPC fixed/generic decode policy.
    #[serde(default)]
    pub lpc_decode: Bd82x6xLpcDecodeConfig,
    /// GPIO pad configuration from coreboot variant `gpio.c`.
    #[serde(default)]
    pub gpio: GpioConfig,
    /// SMBus I/O base.
    #[serde(default = "default_smbus_base")]
    pub smbus_base: u16,
    /// SATA port map. X220 coreboot enables HDD bay, dock, and mSATA: 0x7.
    #[serde(default)]
    pub sata_port_map: u8,
    /// SATA interface speed support. X220 sets 0x3 for 6.0 Gb/s.
    #[serde(default)]
    pub sata_interface_speed_support: u8,
    /// GbE device visibility policy. X220 coreboot enables D25:F0.
    #[serde(default = "default_true")]
    pub gbe_enabled: bool,
    /// Optional Lenovo AT24RF08C EEPROM/RFID lock address.
    #[serde(default)]
    pub eeprom_lock_addr: Option<u8>,
    /// PCIe root ports 1..8 enabled.
    #[serde(default)]
    pub pcie_ports: [bool; 8],
    /// PCIe hotplug map from coreboot devicetree.
    #[serde(default)]
    pub pcie_hotplug_map: [bool; 8],
    /// Enable zero-based linear PCIe root port functions.
    #[serde(default)]
    pub pcie_port_coalesce: bool,
    /// SPI upper/lower VSCC values. X220 sets both to 0x2005.
    #[serde(default)]
    pub spi_uvscc: u16,
    #[serde(default)]
    pub spi_lvscc: u16,
    /// Optional HD Audio verb table from board RON.
    #[serde(default)]
    pub hda: Option<fstart_hda::HdaConfig>,
    /// ACPI device name, normally LPCB for the PCH LPC bridge.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
    /// Optional board-provided late RCBA raw routing.
    #[serde(default)]
    pub late_rcba: Option<Bd82x6xLateRcbaConfig>,
}

fn default_ecam_base() -> u64 {
    0xe000_0000
}
fn default_smbus_base() -> u16 {
    0x0400
}

fn default_true() -> bool {
    true
}

/// Intel Cougar Point PCH driver.
pub struct IntelBd82x6x {
    config: &'static IntelBd82x6xConfig,
    smbus: Option<I801SmBus>,
}

// SAFETY: firmware performs chipset init on the BSP before concurrency exists.
unsafe impl Send for IntelBd82x6x {}
// SAFETY: the struct contains immutable config and fixed I/O-base bus state.
unsafe impl Sync for IntelBd82x6x {}

impl IntelBd82x6x {
    fn lpc(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(0, pch::LPC_DEV, pch::LPC_FUNC)
    }

    fn lpc_regs(&self) -> &'static Bd82x6xLpcPciConfig {
        // SAFETY: the LPC bridge is fixed at 00:1f.0, ECAM is initialized by
        // the Sandy Bridge host bridge, and this overlay covers documented
        // chipset config registers. Exact BAR-enabling sequences stay raw where
        // needed.
        unsafe { self.lpc().regs::<Bd82x6xLpcPciConfig>() }
    }

    fn smbus_regs(&self) -> &'static Bd82x6xSmbusPciConfig {
        // SAFETY: the SMBus controller is fixed at 00:1f.3 and the overlay is
        // limited to the standard header plus the bd82x6x-specific 0x80 gate.
        unsafe { ecam::EcamDevice::new(0, pch::SMBUS_DEV, pch::SMBUS_FUNC).regs() }
    }

    fn rcba(&self) -> Rcba {
        Rcba::new((self.config.rcba & 0xffff_c000) as usize)
    }

    fn enable_spi_prefetching_and_caching(&self) {
        // Match coreboot `enable_spi_prefetching_and_caching()` before any
        // extended memory-mapped SPI reads: LPC config 0xdc bits [3:2] = 10.
        self.lpc_regs()
            .bios_cntl
            .modify(BIOS_CNTL_REG::PREFETCH_CACHE_CTRL.val(2));
    }

    fn program_fixed_bars(&self) {
        let lpc = self.lpc_regs();
        lpc.rcba.set((self.config.rcba as u32 & 0xffff_c000) | 1);
        lpc.pmbase.set(u32::from(pch::DEFAULT_PMBASE) | 1);
        lpc.acpi_cntl.modify(ACPI_CNTL_REG::ACPI_EN::SET);
        lpc.gpio_base.set(u32::from(pch::DEFAULT_GPIOBASE) | 1);
        lpc.gpio_cntl.modify(GPIO_CNTL_REG::GPIO_EN::SET);
        lpc.command.modify(PCI_COMMAND_BITS::IO_SPACE::SET);
    }

    fn reset_watchdog_and_cmos(&self) {
        // Coreboot bd82x6x bootblock enables the upper 128 bytes of CMOS via
        // RCBA RC bit 2, sets GCS NO_REBOOT, and halts/clears TCO.
        let rcba = self.rcba();
        rcba.regs().rc.modify(RC_REG::EXTENDED_CMOS_ENABLE::SET);
        rcba.regs().gcs.modify(GCS_REG::NO_REBOOT::SET);
        let tco = PmIo::new(pch::DEFAULT_PMBASE).tco();
        let cnt = tco.read16(pmio::TCO1_CNT);
        tco.write16(pmio::TCO1_CNT, cnt | (1 << 11));
        tco.write16(pmio::TCO1_STS, 1 << 3);
        tco.write16(pmio::TCO2_STS, 1 << 1);
    }

    fn program_spi_speed_from_descriptor(&self) {
        // Coreboot bootblock selects software-sequence SPI speed from the flash
        // descriptor's component section before doing larger SPI reads.
        let rcba = self.rcba();
        rcba.regs().fdoc.set(0x1000);
        let fdod = rcba.regs().fdod.read(FDOD_REG::COMPONENT_FREQ) as u8;
        rcba.regs().ssfc.modify(SSFC_REG::COMPONENT_FREQ.val(fdod));
    }

    fn program_spi_vscc(&self) {
        if self.config.spi_uvscc != 0 || self.config.spi_lvscc != 0 {
            let rcba = self.rcba();
            rcba.regs().uvscc.set(u32::from(self.config.spi_uvscc));
            rcba.regs().lvscc.set(u32::from(self.config.spi_lvscc));
            rcba.regs()
                .lvscc
                .set(u32::from(self.config.spi_lvscc) | (1 << 23));
        }
    }

    fn program_default_interrupt_map(&self) {
        // Coreboot `southbridge_configure_default_intmap()` defaults.
        let rcba = self.rcba();
        rcba.regs().d31ip.set(0x0430_2100);
        rcba.regs().d29ip.set(0x0000_0001);
        rcba.regs().d28ip.set(0x4321_4321);
        rcba.regs().d27ip.set(0x0000_0001);
        rcba.regs().d26ip.set(0x0000_0001);
        rcba.regs().d25ip.set(0x0000_0001);
        rcba.regs().d22ip.set(0x0000_0001);
        rcba.regs().d20ip.set(0x0000_0001);
        rcba.regs().d31ir.set(0x1100);
        rcba.regs().d29ir.set(0x0002);
        rcba.regs().d28ir.set(0x3210);
        rcba.regs().d27ir.set(0x0003);
        rcba.regs().d26ir.set(0x0003);
        rcba.regs().d25ir.set(0x0001);
        rcba.regs().d22ir.set(0x0000);
        rcba.regs().d20ir.set(0x0001);
        rcba.regs().oic.set(0x0100);
        rcba.regs().fd.set(pch::PCH_DISABLE_ALWAYS);
    }

    fn program_pirq_routes(&self) {
        let lpc = self.lpc_regs();
        for (reg, route) in lpc
            .pirq_rout
            .iter()
            .chain(lpc.pirqe_rout.iter())
            .zip(self.config.pirq_routing.iter().copied())
        {
            reg.set(route);
        }
    }

    fn program_lpc_decode(&self) {
        let lpc = self.lpc_regs();
        // Coreboot bd82x6x uses LPC_IO_DEC plus LPC_EN and GEN_DEC registers.
        // 0x0010 selects COMA 0x3f8 and COMB 0x2f8.  Coreboot enables the
        // common fixed LPC ranges too: 0x2e/0x2f, 0x4e/0x4f, EC/KBC
        // 0x62/0x66 and 0x60/0x64, FDD, LPT, COMB, and COMA.
        match self.config.lpc_decode.fixed {
            LpcFixedDecode::Com1Com2 => {
                lpc.lpc_io_dec.set(0x0010);
                lpc.lpc_en.set(pch::LPC_EN_ALL);
            }
        }
        lpc.serirq_cntl.set(0xd0);
        for (idx, value) in self.config.lpc_decode.gen_dec.iter().copied().enumerate() {
            lpc.gen_dec[idx].set(value);
        }
    }

    fn setup_gpio(&self) {
        IchGpio::new(pch::DEFAULT_GPIOBASE).setup(&self.config.gpio);
    }

    fn setup_smbus(&mut self) {
        // bd82x6x `pch_smbus_init()`: disable SMBus clock-gating bits that can
        // break early transactions, then set the receive-slave address.
        self.smbus_regs().cfg80.modify(
            SMBUS_CFG80_REG::BIT8::CLEAR
                + SMBUS_CFG80_REG::BIT10::CLEAR
                + SMBUS_CFG80_REG::BIT12::CLEAR
                + SMBUS_CFG80_REG::BIT14::CLEAR,
        );
        let bus =
            I801SmBus::enable_on_i801(0, pch::SMBUS_DEV, pch::SMBUS_FUNC, self.config.smbus_base);
        bus.set_receive_slave_addr(pch::SMBUS_SLAVE_ADDR);
        self.smbus = Some(bus);
    }

    fn program_gbe_buc(&self) {
        let rcba = self.rcba();
        let current = rcba.regs().buc.get();
        if self.config.gbe_enabled {
            rcba.regs().buc.modify(BUC_REG::DISABLE_GBE::CLEAR);
        } else {
            rcba.regs().buc.modify(BUC_REG::DISABLE_GBE::SET);
        }
        if rcba.regs().buc.get() != current {
            self.full_reset();
        }
    }

    fn full_reset(&self) -> ! {
        self.lpc_regs()
            .etr3
            .modify(ETR3_REG::CWORWRE::CLEAR + ETR3_REG::CF9GR::CLEAR);
        // SAFETY: BUC changed and coreboot performs a CF9 full reset here.
        unsafe {
            fstart_pio::outb(0x0cf9, 0x0a);
            fstart_pio::outb(0x0cf9, 0x0e);
        }
        loop {
            core::hint::spin_loop();
        }
    }

    fn lock_lenovo_eeprom(&mut self) {
        let Some(addr) = self.config.eeprom_lock_addr else {
            return;
        };
        if self.smbus.is_none() {
            self.setup_smbus();
        }
        for offset in 0..8u8 {
            for _ in 0..100 {
                if self
                    .smbus
                    .as_mut()
                    .map(|bus| bus.write_byte(addr, offset, 0x0f).is_ok())
                    .unwrap_or(false)
                {
                    break;
                }
            }
        }
    }

    fn program_gpi_routing(&self) {
        let mut gpio_rout = 0u32;
        for (idx, route) in self.config.gpi_routing.iter().enumerate() {
            gpio_rout |= ((*route as u32) & 0x03) << (idx * 2);
        }
        self.lpc_regs().gpio_rout.set(gpio_rout);
        #[cfg(target_arch = "x86_64")]
        // SAFETY: PMBASE was programmed by `program_fixed_bars()` and the
        // offsets match coreboot bd82x6x `GPE0_EN` and `ALT_GP_SMI_EN`.
        unsafe {
            fstart_pio::outl(pch::DEFAULT_PMBASE + pch::GPE0_EN, self.config.gpe0_en);
            fstart_pio::outw(
                pch::DEFAULT_PMBASE + pch::ALT_GP_SMI_EN,
                self.config.alt_gp_smi_en,
            );
        }
    }

    fn program_late_rcba(&self) {
        if let Some(r) = self.config.late_rcba {
            // Device interrupt pin registers in RCBA.  These offsets match the
            // coreboot-style DxxIP names; the board RON keeps raw values.
            let rcba = self.rcba();
            rcba.regs().d31ip.set(r.d31ip);
            rcba.regs().d29ip.set(r.d29ip);
            rcba.regs().d28ip.set(r.d28ip);
            rcba.regs().d27ip.set(r.d27ip);
            rcba.regs().d26ip.set(r.d26ip);
            rcba.regs().d25ip.set(r.d25ip);
        }
    }
}

impl Device for IntelBd82x6x {
    const NAME: &'static str = "intel-bd82x6x";
    const COMPATIBLE: &'static [&'static str] = &["intel,bd82x6x", "intel,cougar-point-pch"];
    type Config = IntelBd82x6xConfig;

    fn new(config: &'static Self::Config) -> Result<Self, DeviceError> {
        Ok(Self {
            config,
            smbus: None,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

impl PreConsoleInit for IntelBd82x6x {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        <Self as Southbridge>::pre_console_init(self)
    }
}

impl EarlyInit for IntelBd82x6x {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        <Self as Southbridge>::early_init(self)
    }
}

impl PostDramInit for IntelBd82x6x {
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        <Self as Southbridge>::ramstage_init(self)
    }
}

impl FinalizeInit for IntelBd82x6x {
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        <Self as Southbridge>::finalize(self)
    }
}

impl Southbridge for IntelBd82x6x {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.enable_spi_prefetching_and_caching();
        self.program_fixed_bars();
        self.reset_watchdog_and_cmos();
        self.program_spi_speed_from_descriptor();
        self.program_default_interrupt_map();
        self.program_pirq_routes();
        self.program_lpc_decode();
        self.setup_gpio();
        Ok(())
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.enable_spi_prefetching_and_caching();
        self.program_fixed_bars();
        self.reset_watchdog_and_cmos();
        self.program_gbe_buc();
        self.program_spi_speed_from_descriptor();
        self.program_default_interrupt_map();
        self.program_pirq_routes();
        self.program_lpc_decode();
        self.setup_gpio();
        self.program_gpi_routing();
        self.setup_smbus();
        self.lock_lenovo_eeprom();
        Ok(())
    }

    fn gpio_get(&self, pin: u32) -> Result<bool, ServiceError> {
        Ok(IchGpio::new(pch::DEFAULT_GPIOBASE).get(pin as u8))
    }

    fn gpio_set(&self, pin: u32, value: bool) -> Result<(), ServiceError> {
        IchGpio::new(pch::DEFAULT_GPIOBASE).set(pin as u8, value);
        Ok(())
    }

    fn ramstage_init(&mut self) -> Result<(), ServiceError> {
        self.program_late_rcba();
        self.program_spi_vscc();
        fstart_log::info!("bd82x6x: ramstage policy recorded (SATA/USB/HDA TODO)");
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        // TODO: port bd82x6x SPI/BIOS_CNTL/TCO/SMI lock-down once SMM policy is
        // in place.  Keep this hook explicit so board stages match coreboot.
        Ok(())
    }
}

impl SmBus for IntelBd82x6x {
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        if self.smbus.is_none() {
            self.setup_smbus();
        }
        self.smbus
            .as_ref()
            .ok_or(ServiceError::NotInitialized)?
            .read_byte_data(addr, cmd)
    }

    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
        if self.smbus.is_none() {
            self.setup_smbus();
        }
        self.smbus
            .as_ref()
            .ok_or(ServiceError::NotInitialized)?
            .write_byte_data(addr, cmd, value)
    }
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for IntelBd82x6x {
        type Config = IntelBd82x6xConfig;

        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let rcba = config.rcba as u32;
            let pmbase = pch::DEFAULT_PMBASE as u32;
            let gpiobase = pch::DEFAULT_GPIOBASE as u32;
            let pmio_base = pch::DEFAULT_PMBASE;
            let gpio_base = pch::DEFAULT_GPIOBASE;

            let mut aml = acpi_dsl! {
                Scope("\\") {
                    OperationRegion("PMIO", SystemIO, #{pmbase}, 0x80u32);
                    Field("PMIO", ByteAcc, NoLock, Preserve) {
                        Offset(0x20),
                        , 16,
                        GS00, 1, GS01, 1, GS02, 1, GS03, 1,
                        GS04, 1, GS05, 1, GS06, 1, GS07, 1,
                        GS08, 1, GS09, 1, GS10, 1, GS11, 1,
                        GS12, 1, GS13, 1, GS14, 1, GS15, 1,
                        Offset(0x28),
                        , 16,
                        GE00, 1, GE01, 1, GE02, 1, GE03, 1,
                        GE04, 1, GE05, 1, GE06, 1, GE07, 1,
                        GE08, 1, GE09, 1, GE10, 1, GE11, 1,
                        GE12, 1, GE13, 1, GE14, 1, GE15, 1,
                        Offset(0x42),
                        , 1,
                        GPEC, 1,
                    }
                    OperationRegion("GPIO", SystemIO, #{gpiobase}, 0x6Cu32);
                    Field("GPIO", ByteAcc, NoLock, Preserve) {
                        Offset(0x0C),
                        GP00, 1, GP01, 1, GP02, 1, GP03, 1,
                        GP04, 1, GP05, 1, GP06, 1, GP07, 1,
                        GP08, 1, GP09, 1, GP10, 1, GP11, 1,
                        GP12, 1, GP13, 1, GP14, 1, GP15, 1,
                        GP16, 1, GP17, 1, GP18, 1, GP19, 1,
                        GP20, 1, GP21, 1, GP22, 1, GP23, 1,
                        GP24, 1, GP25, 1, GP26, 1, GP27, 1,
                        GP28, 1, GP29, 1, GP30, 1, GP31, 1,
                        Offset(0x18),
                        GB00, 1, GB01, 1, GB02, 1, GB03, 1,
                        GB04, 1, GB05, 1, GB06, 1, GB07, 1,
                        GB08, 1, GB09, 1, GB10, 1, GB11, 1,
                        GB12, 1, GB13, 1, GB14, 1, GB15, 1,
                        GB16, 1, GB17, 1, GB18, 1, GB19, 1,
                        GB20, 1, GB21, 1, GB22, 1, GB23, 1,
                        GB24, 1, GB25, 1, GB26, 1, GB27, 1,
                        GB28, 1, GB29, 1, GB30, 1, GB31, 1,
                        Offset(0x2C),
                        GIV0, 8, GIV1, 8, GIV2, 8, GIV3, 8,
                        Offset(0x38),
                        GP32, 1, GP33, 1, GP34, 1, GP35, 1,
                        GP36, 1, GP37, 1, GP38, 1, GP39, 1,
                        GP40, 1, GP41, 1, GP42, 1, GP43, 1,
                        GP44, 1, GP45, 1, GP46, 1, GP47, 1,
                        GP48, 1, GP49, 1, GP50, 1, GP51, 1,
                        GP52, 1, GP53, 1, GP54, 1, GP55, 1,
                        GP56, 1, GP57, 1, GP58, 1, GP59, 1,
                        GP60, 1, GP61, 1, GP62, 1, GP63, 1,
                        Offset(0x48),
                        GP64, 1, GP65, 1, GP66, 1, GP67, 1,
                        GP68, 1, GP69, 1, GP70, 1, GP71, 1,
                        GP72, 1, GP73, 1, GP74, 1, GP75, 1,
                    }
                }
            };

            aml.extend_from_slice(&acpi_dsl! {
                Scope("\\_SB_.PCI0") {
                    OperationRegion("RCRB", SystemMemory, #{rcba}, 0x4000u32);
                    Field("RCRB", DWordAcc, Lock, Preserve) {
                        Offset(0x3404),
                        HPAS, 2, , 5, HPTE, 1,
                        Offset(0x3418),
                        , 1, PCID, 1, SA1D, 1, SMBD, 1, HDAD, 1,
                        , 8, EH2D, 1, LPBD, 1, EH1D, 1,
                        RP1D, 1, RP2D, 1, RP3D, 1, RP4D, 1,
                        RP5D, 1, RP6D, 1, RP7D, 1, RP8D, 1,
                        TTRD, 1, SA2D, 1,
                        Offset(0x3428),
                        BDFD, 1, ME1D, 1, ME2D, 1, IDRD, 1, KTCT, 1,
                    }

                    Device("HDEF") { Name("_ADR", 0x001B0000u32); Name("_PRW", Package(13u32, 4u32)); }

                    Device("RP01") { Name("_ADR", 0x001C0000u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 16u32), Package(0x0000FFFFu32, 1u32, 0u32, 17u32), Package(0x0000FFFFu32, 2u32, 0u32, 18u32), Package(0x0000FFFFu32, 3u32, 0u32, 19u32))); }
                    Device("RP02") { Name("_ADR", 0x001C0001u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 17u32), Package(0x0000FFFFu32, 1u32, 0u32, 18u32), Package(0x0000FFFFu32, 2u32, 0u32, 19u32), Package(0x0000FFFFu32, 3u32, 0u32, 16u32))); }
                    Device("RP03") { Name("_ADR", 0x001C0002u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 18u32), Package(0x0000FFFFu32, 1u32, 0u32, 19u32), Package(0x0000FFFFu32, 2u32, 0u32, 16u32), Package(0x0000FFFFu32, 3u32, 0u32, 17u32))); }
                    Device("RP04") { Name("_ADR", 0x001C0003u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 19u32), Package(0x0000FFFFu32, 1u32, 0u32, 16u32), Package(0x0000FFFFu32, 2u32, 0u32, 17u32), Package(0x0000FFFFu32, 3u32, 0u32, 18u32))); }
                    Device("RP05") { Name("_ADR", 0x001C0004u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 16u32), Package(0x0000FFFFu32, 1u32, 0u32, 17u32), Package(0x0000FFFFu32, 2u32, 0u32, 18u32), Package(0x0000FFFFu32, 3u32, 0u32, 19u32))); }
                    Device("RP06") { Name("_ADR", 0x001C0005u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 17u32), Package(0x0000FFFFu32, 1u32, 0u32, 18u32), Package(0x0000FFFFu32, 2u32, 0u32, 19u32), Package(0x0000FFFFu32, 3u32, 0u32, 16u32))); }
                    Device("RP07") { Name("_ADR", 0x001C0006u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 18u32), Package(0x0000FFFFu32, 1u32, 0u32, 19u32), Package(0x0000FFFFu32, 2u32, 0u32, 16u32), Package(0x0000FFFFu32, 3u32, 0u32, 17u32))); }
                    Device("RP08") { Name("_ADR", 0x001C0007u32); Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 19u32), Package(0x0000FFFFu32, 1u32, 0u32, 16u32), Package(0x0000FFFFu32, 2u32, 0u32, 17u32), Package(0x0000FFFFu32, 3u32, 0u32, 18u32))); }

                    Device("EHC1") { Name("_ADR", 0x001D0000u32); Name("_PRW", Package(13u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("EHC2") { Name("_ADR", 0x001A0000u32); Name("_PRW", Package(13u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("XHC_") {
                        Name("_ADR", 0x00140000u32);
                        OperationRegion("XDEV", PciConfig, 0u32, 256u32);
                        Field("XDEV", DWordAcc, NoLock, Preserve) {
                            Offset(0xD0),
                            X2PR, 32,
                            PRM2, 32,
                            SSEN, 32,
                            RPM3, 32,
                            XPRT, 32,
                        }
                        Name("_PRW", Package(13u32, 4u32));
                        Method("POSC", 2, Serialized) {
                            CreateDwordField(#{fstart_acpi::aml::Arg(1)}, 0u32, "CDW1");
                            If (Arg0 != 1u32) {
                                CDW1 = CDW1 | 8u32;
                            }
                            Return(Arg1);
                        }
                        Method("_S3D", 0, NotSerialized) { Return(2u32); }
                        Method("_S4D", 0, NotSerialized) { Return(2u32); }
                    }
                    Device("SATA") { Name("_ADR", 0x001F0002u32); }
                    Device("SBUS") { Name("_ADR", 0x001F0003u32); }

                    Device("LPCB") {
                        Name("_ADR", 0x001F0000u32);
                        OperationRegion("LPC0", PciConfig, 0x00u32, 0x100u32);
                        Field("LPC0", AnyAcc, NoLock, Preserve) {
                            Offset(0x40), PMBS, 16,
                            Offset(0x60), PRTA, 8, PRTB, 8, PRTC, 8, PRTD, 8,
                            Offset(0x68), PRTE, 8, PRTF, 8, PRTG, 8, PRTH, 8,
                            Offset(0x80), IOD0, 8, IOD1, 8,
                            Offset(0xB8),
                            GR00, 2, GR01, 2, GR02, 2, GR03, 2,
                            GR04, 2, GR05, 2, GR06, 2, GR07, 2,
                            GR08, 2, GR09, 2, GR10, 2, GR11, 2,
                            GR12, 2, GR13, 2, GR14, 2, GR15, 2,
                        }

                        Device("LNKA") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 1u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKB") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 2u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKC") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 3u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKD") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 4u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKE") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 5u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKF") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 6u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKG") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 7u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKH") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 8u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }

                        Device("EC__") {
                            Name("_HID", EisaId("PNP0C09"));
                            Name("_UID", 0u32);
                            Name("_GPE", 17u32);
                            Name("_CRS", ResourceTemplate {
                                IO(0x0062u16, 0x0062u16, 0x01u8, 0x01u8);
                                IO(0x0066u16, 0x0066u16, 0x01u8, 0x01u8);
                            });
                            Method("MUTE", 1, NotSerialized) { Return(0u32); }
                            Method("USBP", 1, NotSerialized) { Return(0u32); }
                            Method("RADI", 1, NotSerialized) { Return(0u32); }
                            Device("HKEY") {
                                Name("_HID", EisaId("IBM0068"));
                                Method("WAKE", 1, NotSerialized) { Return(0u32); }
                            }
                        }

                        Device("DMAC") { Name("_HID", EisaId("PNP0200")); Name("_CRS", ResourceTemplate { IO(0x0000u16, 0x0000u16, 0x01u8, 0x20u8); IO(0x0081u16, 0x0081u16, 0x01u8, 0x11u8); IO(0x0093u16, 0x0093u16, 0x01u8, 0x0Du8); IO(0x00C0u16, 0x00C0u16, 0x01u8, 0x20u8); }); }
                        Device("FWH_") { Name("_HID", EisaId("INT0800")); Name("_CRS", ResourceTemplate { Memory32Fixed(ReadOnly, 0xFF000000u32, 0x01000000u32); }); }
                        Device("HPET") { Name("_HID", EisaId("PNP0103")); Name("_CID", 0x010CD041u32); Name("_CRS", ResourceTemplate { Memory32Fixed(ReadOnly, 0xFED00000u32, 0x400u32); }); }
                        Device("PIC_") { Name("_HID", EisaId("PNP0000")); Name("_CRS", ResourceTemplate { IO(0x0020u16, 0x0020u16, 0x01u8, 0x02u8); IO(0x0024u16, 0x0024u16, 0x01u8, 0x02u8); IO(0x0028u16, 0x0028u16, 0x01u8, 0x02u8); IO(0x002Cu16, 0x002Cu16, 0x01u8, 0x02u8); IO(0x0030u16, 0x0030u16, 0x01u8, 0x02u8); IO(0x0034u16, 0x0034u16, 0x01u8, 0x02u8); IO(0x0038u16, 0x0038u16, 0x01u8, 0x02u8); IO(0x003Cu16, 0x003Cu16, 0x01u8, 0x02u8); IO(0x00A0u16, 0x00A0u16, 0x01u8, 0x02u8); IO(0x00A4u16, 0x00A4u16, 0x01u8, 0x02u8); IO(0x00A8u16, 0x00A8u16, 0x01u8, 0x02u8); IO(0x00ACu16, 0x00ACu16, 0x01u8, 0x02u8); IO(0x00B0u16, 0x00B0u16, 0x01u8, 0x02u8); IO(0x00B4u16, 0x00B4u16, 0x01u8, 0x02u8); IO(0x00B8u16, 0x00B8u16, 0x01u8, 0x02u8); IO(0x00BCu16, 0x00BCu16, 0x01u8, 0x02u8); IO(0x04D0u16, 0x04D0u16, 0x01u8, 0x02u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 2u32); }); }
                        Device("MATH") { Name("_HID", EisaId("PNP0C04")); Name("_CRS", ResourceTemplate { IO(0x00F0u16, 0x00F0u16, 0x01u8, 0x01u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 13u32); }); }
                        Device("LDRC") { Name("_HID", EisaId("PNP0C02")); Name("_UID", 2u32); Name("_CRS", ResourceTemplate { IO(0x002Eu16, 0x002Eu16, 0x01u8, 0x02u8); IO(0x004Eu16, 0x004Eu16, 0x01u8, 0x02u8); IO(0x0061u16, 0x0061u16, 0x01u8, 0x01u8); IO(0x0063u16, 0x0063u16, 0x01u8, 0x01u8); IO(0x0065u16, 0x0065u16, 0x01u8, 0x01u8); IO(0x0067u16, 0x0067u16, 0x01u8, 0x01u8); IO(0x0080u16, 0x0080u16, 0x01u8, 0x01u8); IO(0x0092u16, 0x0092u16, 0x01u8, 0x01u8); IO(0x00B2u16, 0x00B2u16, 0x01u8, 0x02u8); IO(#{pmio_base}, #{pmio_base}, 0x01u8, 0x80u8); IO(#{gpio_base}, #{gpio_base}, 0x01u8, 0x40u8); }); }
                        Device("RTC_") { Name("_HID", EisaId("PNP0B00")); Name("_CRS", ResourceTemplate { IO(0x0070u16, 0x0070u16, 0x01u8, 0x08u8); }); }
                        Device("TIMR") { Name("_HID", EisaId("PNP0100")); Name("_CRS", ResourceTemplate { IO(0x0040u16, 0x0040u16, 0x01u8, 0x04u8); IO(0x0050u16, 0x0050u16, 0x10u8, 0x04u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 0u32); }); }
                        Device("PS2K") { Name("_HID", EisaId("PNP0303")); Name("_CID", EisaId("PNP030B")); Name("_CRS", ResourceTemplate { IO(0x0060u16, 0x0060u16, 0x01u8, 0x01u8); IO(0x0064u16, 0x0064u16, 0x01u8, 0x01u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 1u32); }); Method("_STA", 0, NotSerialized) { Return(0x0Fu32); } }
                        Device("PS2M") { Name("_HID", EisaId("PNP0F13")); Name("_CRS", ResourceTemplate { Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 12u32); }); Method("_STA", 0, NotSerialized) { Return(0x0Fu32); } }
                    }

                    Method("_OSC", 4, NotSerialized) {
                        If (Arg0 == ToUUID("7c9512a9-1705-4cb4-af7d-506a2423ab71")) {
                            Return(XHC_.POSC(Arg2, Arg3));
                        }
                        If (Arg0 == ToUUID("33DB4D5B-1FF7-401C-9657-7441C03DD766")) {
                            Return(Arg3);
                        }
                        CreateDwordField(#{fstart_acpi::aml::Arg(3)}, 0u32, "CDW1");
                        CDW1 = CDW1 | 4u32;
                        Return(Arg3);
                    }
                }
            });

            aml
        }
    }
}
