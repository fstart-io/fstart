//! ICH9-and-later SPI controller register block (RCBA + 0x3800 on ICH9 and
//! the 6/7-series PCH): the software-sequencing opcode menu, the vendor
//! component capabilities and the lockdown (coreboot
//! `southbridge/intel/common/spi.c`). Flash access itself is [`super::spi`].

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u16,
    pub HSFS [
        FDONE OFFSET(0) NUMBITS(1) [],
        FCERR OFFSET(1) NUMBITS(1) [],
        AEL OFFSET(2) NUMBITS(1) [],
        SCIP OFFSET(5) NUMBITS(1) [],
        FDOPSS OFFSET(13) NUMBITS(1) [],
        FDV OFFSET(14) NUMBITS(1) [],
        FLOCKDN OFFSET(15) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    pub FREG [
        BASE OFFSET(0) NUMBITS(13) [],
        LIMIT OFFSET(16) NUMBITS(13) []
    ],
    pub VSCC [
        LOCK OFFSET(23) NUMBITS(1) []
    ]
];

register_structs! {
    /// The SPI controller registers.
    pub SpiRegs {
        (0x00 => pub bfpr: MmioReadOnly<u32>),
        (0x04 => pub hsfs: MmioReadWrite<u16, HSFS::Register>),
        (0x06 => pub hsfc: MmioReadWrite<u16>),
        (0x08 => pub faddr: MmioReadWrite<u32>),
        (0x0c => _reserved0),
        (0x10 => pub fdata: [MmioReadWrite<u32>; 16]),
        (0x50 => pub frap: MmioReadWrite<u32>),
        (0x54 => pub freg: [MmioReadOnly<u32, FREG::Register>; 5]),
        (0x68 => _reserved1),
        (0x74 => pub pr: [MmioReadWrite<u32>; 5]),
        (0x88 => _reserved2),
        (0x90 => pub ssfs: MmioReadWrite<u8>),
        (0x91 => pub ssfc: [MmioReadWrite<u8>; 3]),
        (0x94 => pub preop: MmioReadWrite<u16>),
        (0x96 => pub optype: MmioReadWrite<u16>),
        (0x98 => pub opmenu: [MmioReadWrite<u8>; 8]),
        (0xa0 => _reserved3),
        (0xb0 => pub fdoc: MmioReadWrite<u32>),
        (0xb4 => pub fdod: MmioReadOnly<u32>),
        (0xb8 => _reserved4),
        (0xc0 => pub afc: MmioReadWrite<u32>),
        (0xc4 => pub lvscc: MmioReadWrite<u32, VSCC::Register>),
        (0xc8 => pub uvscc: MmioReadWrite<u32, VSCC::Register>),
        (0xcc => _reserved5),
        (0xf0 => pub soft_reset_lock: MmioReadWrite<u32>),
        (0xf4 => pub soft_reset_ctrl: MmioReadWrite<u32>),
        (0xf8 => pub soft_reset_data: MmioReadWrite<u32>),
        (0xfc => _reserved6),
        (0x200 => @END),
    }
}

/// Software-sequencing opcode types (`OPTYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpType {
    ReadNoAddr = 0,
    WriteNoAddr = 1,
    ReadWithAddr = 2,
    WriteWithAddr = 3,
}

/// The software-sequencing configuration (coreboot `intel_swseq_spi_config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwseqConfig {
    pub prefixes: [u8; 2],
    pub ops: [(u8, OpType); 8],
}

impl SwseqConfig {
    /// coreboot `spi_finalize_ops` defaults.
    pub const DEFAULT: Self = Self {
        prefixes: [0x06, 0x50],
        ops: [
            (0x01, OpType::WriteNoAddr),
            (0x02, OpType::WriteWithAddr),
            (0x03, OpType::ReadWithAddr),
            (0x05, OpType::ReadNoAddr),
            (0x20, OpType::WriteWithAddr),
            (0x9f, OpType::ReadNoAddr),
            (0xd8, OpType::WriteWithAddr),
            (0x0b, OpType::ReadWithAddr),
        ],
    };
}

impl SpiRegs {
    /// Whether `FLOCKDN` froze the configuration.
    #[must_use]
    pub fn locked(&self) -> bool {
        self.hsfs.is_set(HSFS::FLOCKDN)
    }

    /// coreboot `spi_finalize_ops`: program the software-sequencing opcode
    /// menu the OS may use once the controller is locked.
    pub fn finalize_ops(&self, config: &SwseqConfig) {
        if self.locked() {
            return;
        }
        self.preop
            .set(u16::from(config.prefixes[0]) | u16::from(config.prefixes[1]) << 8);
        let mut optype = 0u16;
        for (i, (opcode, kind)) in config.ops.iter().enumerate() {
            optype |= (*kind as u16) << (2 * i);
            self.opmenu[i].set(*opcode);
        }
        self.optype.set(optype);
    }

    /// Program the vendor component capabilities and lock them.
    pub fn set_vscc(&self, upper: u32, lower: u32) {
        if upper != 0 {
            self.uvscc.set(upper);
        }
        if lower != 0 {
            self.lvscc.set(lower);
        }
        if upper != 0 || lower != 0 {
            self.lvscc.modify(VSCC::LOCK::SET);
        }
    }

    /// Set `FLOCKDN`.
    pub fn lock(&self) {
        self.hsfs.modify(HSFS::FLOCKDN::SET);
    }
}
