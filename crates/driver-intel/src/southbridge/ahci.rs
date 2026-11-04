//! The AHCI HBA registers ICH SATA setup programs through ABAR (BAR5): the
//! generic host control block and the per-port command register (AHCI 1.x;
//! VSP is Intel's vendor-specific register at 0xa0).

use fstart_core::mmio::MmioReadWrite;
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u32,
    /// CAP, HBA capabilities (R/WO on ICH).
    pub CAP [
        /// Supports external SATA.
        SXS OFFSET(5) NUMBITS(1) [],
        /// Enclosure management.
        EMS OFFSET(6) NUMBITS(1) [],
        /// Command completion coalescing.
        CCCS OFFSET(7) NUMBITS(1) [],
        /// Partial state capable.
        PSC OFFSET(13) NUMBITS(1) [],
        /// Slumber state capable.
        SSC OFFSET(14) NUMBITS(1) [],
        /// Port multiplier.
        PMS OFFSET(17) NUMBITS(1) [],
        /// Aggressive link power management.
        SALP OFFSET(26) NUMBITS(1) [],
        /// Staggered spin-up.
        SSS OFFSET(27) NUMBITS(1) [],
    ],
    /// GHC, global HBA control.
    pub GHC [
        /// AHCI enable: no other ABAR register may be accessed before it.
        AE OFFSET(31) NUMBITS(1) [],
    ],
    /// VSP, Intel vendor specific.
    pub VSP [
        /// SATA link power-down disable.
        SLPD OFFSET(0) NUMBITS(1) [],
    ],
    /// PxCMD, port command and status.
    pub PXCMD [
        /// Hot-plug capable port (R/WO).
        HPCP OFFSET(18) NUMBITS(1) [],
    ],
];

register_structs! {
    pub AhciPort {
        (0x00 => _reserved0: [u8; 0x18]),
        (0x18 => pub cmd: MmioReadWrite<u32, PXCMD::Register>),
        (0x1c => _reserved1: [u8; 0x64]),
        (0x80 => @END),
    },
    pub AhciRegs {
        (0x000 => pub cap: MmioReadWrite<u32, CAP::Register>),
        (0x004 => pub ghc: MmioReadWrite<u32, GHC::Register>),
        (0x008 => _is: [u8; 4]),
        /// PI, ports implemented (R/WO).
        (0x00c => pub pi: MmioReadWrite<u32>),
        (0x010 => _reserved0: [u8; 0x90]),
        (0x0a0 => pub vsp: MmioReadWrite<u32, VSP::Register>),
        (0x0a4 => _reserved1: [u8; 0x5c]),
        (0x100 => pub ports: [AhciPort; 6]),
        (0x400 => @END),
    }
}
