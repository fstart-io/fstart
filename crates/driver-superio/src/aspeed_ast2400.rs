//! ASPEED AST2400/AST2500 BMC Super I/O.
//!
//! Configuration mode is entered with
//! `0xA5` twice and left with a raw `0xAA` to the index port. Besides the
//! usual LDNs the BMC exposes an iLPC2AHB bridge (LDN 0xd) through which the
//! host reads and writes BMC registers, e.g. the SCU pin multiplexing.
//!
//! LDNs: SUART1=2, SUART2=3, SWC=4, KBC=5, GPIO=7, SUART3=0xb, SUART4=0xc,
//! iLPC2AHB=0xd, mailbox=0xe.

use tock_registers::fields::FieldValue;
use tock_registers::register_bitfields;

use crate::{IoResource, SuperIo, SuperIoChip};

pub use crate::{ComPortConfig, LpcBaseProvider, SuperIoConfig};

pub const SUART1: u8 = 0x2;
pub const SUART2: u8 = 0x3;
pub const SWC: u8 = 0x4;
pub const KBC: u8 = 0x5;
pub const GPIO: u8 = 0x7;
pub const SUART3: u8 = 0xb;
pub const SUART4: u8 = 0xc;
pub const ILPC2AHB: u8 = 0xd;
pub const MAILBOX: u8 = 0xe;

/// The Super I/O config port the BMCs answer on.
pub const CONFIG_PORT: u16 = 0x2e;

/// BMC System Control Unit, reachable through iLPC2AHB.
pub const SCU_BASE: u32 = 0x1e6e_2000;
/// SCU00: protection key.
pub const SCU00: u32 = SCU_BASE;
/// SCU84: multi-function pin control #1.
pub const SCU84: u32 = SCU_BASE + 0x84;
/// SCU90: multi-function pin control #5.
pub const SCU90: u32 = SCU_BASE + 0x90;

register_bitfields![u32,
    pub Scu00 [
        KEY OFFSET(0) NUMBITS(32) []
    ],
    pub Scu84 [
        /// UART1 pin functions: TXD1, RXD1 and the modem lines.
        UART1 OFFSET(16) NUMBITS(8) []
    ],
    pub Scu90 [
        /// Cleared by the vendor AST2500 PEIMs with the UART1 routing.
        BITS_5_4 OFFSET(4) NUMBITS(2) []
    ]
];

/// Writing this to SCU00 unlocks the other SCU registers.
pub const SCU_PROTECTION_KEY: u32 = 0x1688_a8a8;

/// iLPC2AHB registers of LDN 0xd.
const AHB_ADDRESS: u8 = 0xf0; // 0xf0..0xf3, big endian
const AHB_DATA: u8 = 0xf4; // 0xf4..0xf7, big endian
const AHB_ACCESS: u8 = 0xf8; // [1:0] width, 2 = 32-bit
const AHB_TRIGGER: u8 = 0xfe;
const AHB_WRITE_TRIGGER: u8 = 0xcf;

/// Zero-sized chip descriptor for the AST2400/AST2500 Super I/O.
pub struct Ast2400Chip;

impl SuperIoChip for Ast2400Chip {
    const ENTER_SEQ: &'static [u8] = &[0xa5, 0xa5];
    const EXIT_REG: u8 = 0x00;
    const EXIT_VAL: u8 = 0x00;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    // No ID check: the BMC firmware controls these registers.
    const CHIP_ID: u16 = 0;
    const COM1_LDN: Option<u8> = Some(SUART1);
    const COM2_LDN: Option<u8> = Some(SUART2);
    const KBC_LDN: Option<u8> = Some(KBC);
    const MOUSE_LDN: Option<u8> = None;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = Some(GPIO);
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = None;
}

/// AST2400/AST2500 Super I/O driver.
pub type Ast2400 = SuperIo<Ast2400Chip>;

impl Ast2400 {
    /// Disable logical devices the host must not claim. A BMC uses some of
    /// them itself (SUART4 carries IPMI SOL on the MJ11), so leaving them
    /// decoded garbles its traffic.
    pub fn disable(&self, ldns: &[u8]) {
        self.enter_config();
        for &ldn in ldns {
            self.logical_device(ldn).set_enabled(false);
        }
        self.exit_config();
    }

    /// Enable a UART at `io_base`/`irq`, with its MSC0 register (0xf0)
    /// explicitly set instead of left to the BMC.
    pub fn enable_uart(&self, ldn: u8, io_base: u16, irq: u8, msc0: u8) {
        self.enter_config();
        let mut device = self.logical_device(ldn);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(io_base));
        device.set_irq(crate::IrqResource::Primary(irq));
        device.write(0xf0, msc0);
        device.set_enabled(true);
        self.exit_config();
    }

    /// Unlock the SCU and give the BMC's UART1 pins to SUART1
    /// (SCU84[23:16]), clearing SCU90[5:4], as the vendor AST2500 PEIMs of
    /// the Gigabyte MJ11 and the Tyan S8030 do. The host then reaches the
    /// board's COM header through SUART1.
    pub fn route_uart1_pins(&self) {
        self.ahb_modify32(SCU00, Scu00::KEY.val(SCU_PROTECTION_KEY));
        self.ahb_modify32(SCU84, Scu84::UART1.val(0xff));
        self.ahb_modify32(SCU90, Scu90::BITS_5_4.val(0));
    }

    /// Read-modify-write a 32-bit BMC (AHB) register through iLPC2AHB, the
    /// way the vendor AST2500 PEIM does. Enables the bridge LDN.
    pub fn ahb_modify32<R: tock_registers::RegisterLongName>(
        &self,
        address: u32,
        field: FieldValue<u32, R>,
    ) {
        self.enter_config();
        let _ = self.logical_device(ILPC2AHB);
        self.write_reg(0x30, self.read(0x30) | 1);
        for (i, byte) in address.to_be_bytes().into_iter().enumerate() {
            self.write_reg(AHB_ADDRESS + i as u8, byte);
        }
        self.write_reg(AHB_ACCESS, (self.read(AHB_ACCESS) & !3) | 2);
        let _ = self.read(AHB_TRIGGER);
        let value = u32::from_be_bytes(core::array::from_fn(|i| self.read(AHB_DATA + i as u8)));
        for (i, byte) in field.modify(value).to_be_bytes().into_iter().enumerate() {
            self.write_reg(AHB_DATA + i as u8, byte);
        }
        self.write_reg(AHB_TRIGGER, AHB_WRITE_TRIGGER);
        self.exit_config();
    }

    fn read(&self, reg: u8) -> u8 {
        use tock_registers::interfaces::Readable;
        self.register::<()>(reg).get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vendor PEIMs' and/or pairs.
    #[test]
    fn uart1_routing_masks() {
        fn and_or<R: tock_registers::RegisterLongName>(f: FieldValue<u32, R>) -> (u32, u32) {
            (!f.mask(), f.value)
        }
        assert_eq!(
            and_or(Scu00::KEY.val(SCU_PROTECTION_KEY)),
            (0, SCU_PROTECTION_KEY)
        );
        assert_eq!(and_or(Scu84::UART1.val(0xff)), (0xff00_ffff, 0x00ff_0000));
        assert_eq!(and_or(Scu90::BITS_5_4.val(0)), (0xffff_ffcf, 0));
    }
}
