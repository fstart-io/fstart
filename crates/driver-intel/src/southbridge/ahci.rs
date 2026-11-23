//! Shared AHCI HBA register layout. Chipset capability policy remains in the
//! family driver; this module neither starts DMA nor allocates command rings.
use fstart_core::mmio::MmioReadWrite;
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u32,
    /// CAP is R/WO on Intel SATA controllers.
    pub CAP [
        NP OFFSET(0) NUMBITS(5) [],
        SXS OFFSET(5) NUMBITS(1) [],
        EMS OFFSET(6) NUMBITS(1) [],
        CCCS OFFSET(7) NUMBITS(1) [],
        PSC OFFSET(13) NUMBITS(1) [],
        SSC OFFSET(14) NUMBITS(1) [],
        PMD OFFSET(15) NUMBITS(1) [],
        PMS OFFSET(17) NUMBITS(1) [],
        ISS OFFSET(20) NUMBITS(4) [],
        SCLO OFFSET(24) NUMBITS(1) [],
        SALP OFFSET(26) NUMBITS(1) [],
        SSS OFFSET(27) NUMBITS(1) [],
        SIS OFFSET(28) NUMBITS(1) []
    ],
    pub GHC [
        HR OFFSET(0) NUMBITS(1) [],
        IE OFFSET(1) NUMBITS(1) [],
        /// No other ABAR register may be accessed before AHCI enable.
        AE OFFSET(31) NUMBITS(1) []
    ],
    pub CAP2 [
        APST OFFSET(2) NUMBITS(1) [],
        SDS OFFSET(3) NUMBITS(1) [],
        SADM OFFSET(4) NUMBITS(1) [],
        DESO OFFSET(5) NUMBITS(1) []
    ],
    /// Intel vendor-specific register at A0h.
    pub VSP [ SLPD OFFSET(0) NUMBITS(1) [] ],
    pub PXCMD [
        ST OFFSET(0) NUMBITS(1) [],
        SUD OFFSET(1) NUMBITS(1) [],
        FRE OFFSET(4) NUMBITS(1) [],
        FR OFFSET(14) NUMBITS(1) [],
        CR OFFSET(15) NUMBITS(1) [],
        HPCP OFFSET(18) NUMBITS(1) [],
        MPSP OFFSET(19) NUMBITS(1) [],
        ESP OFFSET(21) NUMBITS(1) [],
        FBSCP OFFSET(22) NUMBITS(1) [],
        ALPE OFFSET(26) NUMBITS(1) [],
        ASP OFFSET(27) NUMBITS(1) []
    ],
    pub PXDEVSLP [
        DSP OFFSET(1) NUMBITS(1) [],
        DITO OFFSET(15) NUMBITS(10) [],
        DM OFFSET(25) NUMBITS(4) []
    ]
];
register_structs! {
    pub AhciPort {
        (0x00 => _reserved0),
        (0x18 => pub cmd: MmioReadWrite<u32, PXCMD::Register>),
        (0x1c => _reserved1),
        (0x34 => pub sact: MmioReadWrite<u32>),
        (0x38 => pub ci: MmioReadWrite<u32>),
        (0x3c => _reserved2),
        (0x44 => pub devslp: MmioReadWrite<u32, PXDEVSLP::Register>),
        (0x48 => _reserved3),
        (0x80 => @END),
    },
    pub AhciHost {
        (0x000 => pub cap: MmioReadWrite<u32, CAP::Register>),
        (0x004 => pub ghc: MmioReadWrite<u32, GHC::Register>),
        (0x008 => _interrupt_status),
        (0x00c => pub pi: MmioReadWrite<u32>),
        (0x010 => _reserved0),
        (0x024 => pub cap2: MmioReadWrite<u32, CAP2::Register>),
        (0x028 => _reserved1),
        (0x0a0 => pub vsp: MmioReadWrite<u32, VSP::Register>),
        (0x0a4 => _reserved2),
        (0x100 => @END),
    }
}
/// Six ports preserve the existing ICH layout/API; PCH-H binds eight without
/// duplicating the common AHCI header or port definitions.
#[repr(C)]
pub struct AhciRegs<const PORTS: usize = 6> {
    pub host: AhciHost,
    pub ports: [AhciPort; PORTS],
}
impl<const PORTS: usize> core::ops::Deref for AhciRegs<PORTS> {
    type Target = AhciHost;
    fn deref(&self) -> &AhciHost {
        &self.host
    }
}
const _: () = {
    assert!(core::mem::size_of::<AhciRegs>() == 0x400);
    assert!(core::mem::offset_of!(AhciRegs<8>, ports) == 0x100);
    assert!(core::mem::size_of::<AhciRegs<8>>() == 0x500);
};
