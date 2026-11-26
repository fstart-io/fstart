// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH PCIe core, port and root-complex private registers: the one home of
//! their addresses and fields, for NBIO, the MPIO straps and hotplug
//! ([`crate::dxio::wh::straps`], [`crate::dxio::wh::hotplug`]), the early
//! link ([`crate::dxio::wh::early`]) and CXL ([`crate::cxl::wh`]).
//!
//! # Apertures
//!
//! A register is an address in one of five apertures ([`Aperture`]); a
//! port is its PCIe core, the core's RC aperture (the high nibble of the
//! wrapper byte, [`crate::dxio::wh::Wrapper::rc`]) and the port within the
//! core ([`PortId`]):
//!
//! | Aperture | Base | Instance |
//! |---|---|---|
//! | PCIEPORT | `0x1A34_0000` | `core << 20 + port << 12` |
//! | PCIECORE | `0x1A38_0000` | `core << 20` |
//! | RC port (config mirror `0x1C40`, LC `0x1C44`) | `0x1C40_0000` | `rc << 20 + port << 12` |
//! | RC core | `0x1C48_0000` | `rc << 20` |
//! | CNLI | `0x1F10_0000` | `(core & 3) << 16 + port << 13` |
//!
//! # Access
//!
//! Both donors write these registers through MPIO `PCIE_PROXY`
//! ([`crate::dxio::wh::MSG_PCIE_PROXY`]: SilVenicePei
//! `MpioSmnPrivateRegRMWCmn` `0xf64440` behind the MPIO API's `+0x28`
//! read / `+0x30` RMW, AmdNbioDxe `0x24b10`), with these exceptions, which
//! go through host SMN (`xUSLSmnReadModifyWrite` `0xf5c3a0`, SMN read
//! `0xf5c270`): the flit error counter (`0x1C44_02A8`), the NPEM registers
//! (`0x1C40_055C/0560`), CNLI, and the reads of Link Capabilities and Link
//! Status (`0x1C40_0064/0068`; `PcieHotplugInitWh` `0xf62c3d`,
//! `AfterDxioInitPerPortProgramming` `0xf8341f`). So host SMN decodes the
//! RC aperture (NBIO.md Q2); fstart takes each register the vendor's way:
//! [`Reg::access`] is how the vendor writes it, and the two SMN reads are
//! the platform's, on the IOD's SMN. CXL's port and core registers
//! (`CxlFindPortsWh` `0xf6c477..`, `CxlProgramQosWh` `0xf6bff2`) go
//! through the proxy as well.
//!
//! Registers whose bits are named have their fields here; the bits the
//! vendor sets without a name stay raw in the steps that set them, as in
//! the generated tables.

use core::marker::PhantomData;

use tock_registers::{LocalRegisterCopy, RegisterLongName, register_bitfields};

use crate::dxio::rs::mailbox::Error;
use crate::dxio::wh::{Proxy, Width};
use crate::smn::{SmnAccess, SmnReg};

/// The aperture of a register ([module documentation](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aperture {
    /// PCIEPORT `0x1A34_xxxx`: per port of a core.
    Port,
    /// PCIECORE `0x1A38_xxxx`: per core.
    Core,
    /// RC `0x1C40_xxxx`/`0x1C44_xxxx`: per port of an RC aperture.
    RcPort,
    /// RC core `0x1C48_xxxx`: per RC aperture.
    RcCore,
    /// CNLI `0x1F10_xxxx`: per port, CXL.
    Cnli,
}

/// How the vendor writes a register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// MPIO `PCIE_PROXY` (`0x26`).
    Proxy,
    /// Host SMN on the IOD.
    Smn,
}

/// A PCIe port: its core, its core's RC aperture and its port in the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortId {
    pub core: u8,
    pub rc: u8,
    pub port: u8,
}

/// A PCIe core or port register: its address in instance 0, its aperture
/// and the vendor's access path.
pub struct Reg<R: RegisterLongName = ()> {
    base: u32,
    aperture: Aperture,
    access: Access,
    _reg: PhantomData<R>,
}

impl<R: RegisterLongName> Clone for Reg<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R: RegisterLongName> Copy for Reg<R> {}

impl<R: RegisterLongName> Reg<R> {
    pub const fn new(base: u32, aperture: Aperture, access: Access) -> Self {
        Self {
            base,
            aperture,
            access,
            _reg: PhantomData,
        }
    }

    /// A register of the proxy-written apertures.
    const fn proxy(base: u32, aperture: Aperture) -> Self {
        Self::new(base, aperture, Access::Proxy)
    }

    pub const fn base(self) -> u32 {
        self.base
    }

    pub const fn access(self) -> Access {
        self.access
    }

    /// The address of the register of port `p` (of its core for the core
    /// apertures).
    pub const fn address(self, p: PortId) -> u32 {
        let (core, rc, port) = (p.core as u32, p.rc as u32, p.port as u32);
        self.base
            + match self.aperture {
                Aperture::Port => (core << 20) + (port << 12),
                Aperture::Core => core << 20,
                Aperture::RcPort => (rc << 20) + (port << 12),
                Aperture::RcCore => rc << 20,
                Aperture::Cnli => ((core & 3) << 16) + (port << 13),
            }
    }

    /// The register of port `p` (of its core for the core apertures); a
    /// dword register.
    pub const fn at(self, p: PortId) -> SmnReg<R> {
        SmnReg::new(self.address(p))
    }

    /// The register of core `core` with RC aperture `rc` (core apertures).
    pub const fn of_core(self, core: u8, rc: u8) -> SmnReg<R> {
        self.at(PortId { core, rc, port: 0 })
    }
}

register_bitfields![u32,
    /// PCIECORE::PCIE_STRAP_F0 (`0x1A38_02C0`, 32-bit).
    pub PcieStrapF0 [
        AER_EN OFFSET(5) NUMBITS(1) [],
        /// Wrapper flag bit 0 (ACS) on an early-trained or SLT core.
        ACS_EN OFFSET(6) NUMBITS(1) [],
        /// Wrapper flag bit 9 (ECRC check) on an early-trained core.
        ECRC_CHECK_EN OFFSET(13) NUMBITS(1) [],
        /// Wrapper flag bit 8 (ECRC generation) on an early-trained core.
        ECRC_GEN_EN OFFSET(14) NUMBITS(1) []
    ],
    /// PCIECORE `0x1A38_0024` (32-bit): bits 6:0 per core after the
    /// bifurcation (`0xf82a40`), meaning not known.
    pub Core024 [
        VALUE OFFSET(0) NUMBITS(7) []
    ],
    /// PCIECORE `0x1A38_0204` (32-bit).
    pub Core204 [
        /// `CfgRcvErrEnable`.
        RCV_ERR_EN OFFSET(13) NUMBITS(1) []
    ],
    /// PCIECORE `0x1A38_0304` (32-bit).
    pub Core304 [
        /// Wrapper flag bit 7 ? bit 6 : 0 (AER completion-timeout log).
        AER_CTO_LOG OFFSET(6) NUMBITS(1) []
    ],
    /// PCIECORE `0x1A38_0314` (32-bit): CXL error logging.
    pub CxlErrorLog [
        /// 2 with `CxlUnCorrectableErrorLogging`, else 0.
        UNCORRECTABLE OFFSET(0) NUMBITS(8) [],
        /// `CxlCorrectableErrorLogging`.
        CORRECTABLE OFFSET(8) NUMBITS(8) []
    ],
    /// PCIECORE `0x1A38_070C` (32-bit).
    pub Core70c [
        /// 2 with SEV-TIO or IDE capability and no IDE bypass.
        IDE OFFSET(5) NUMBITS(2) []
    ],
    /// PCIECORE `0x1A38_0854` (32-bit).
    pub Core854 [
        /// `PcdMaster7bitSteeringTag`.
        STEERING_TAG_7BIT OFFSET(22) NUMBITS(1) []
    ],
    /// PCIECORE `0x1A38_0008` (32-bit).
    pub Core008 [
        /// `PcdMaster7bitSteeringTag`.
        STEERING_TAG_7BIT OFFSET(4) NUMBITS(1) []
    ],
    /// RC `0x1C44_02E0`/`0x037C`/`0x0418` (32-bit): Gen3/4/5 fixed
    /// transmitter coefficients (`AfterReconfigPerPortProgramming`'s
    /// coefficient paths, DEVICES.md §2.3).
    pub EqCoefficients [
        ENABLE OFFSET(0) NUMBITS(1) [],
        PRE_CURSOR OFFSET(1) NUMBITS(6) [],
        CURSOR OFFSET(7) NUMBITS(6) [],
        POST_CURSOR OFFSET(13) NUMBITS(6) []
    ],
    /// PCIECORE `0x1A38_04E0` (32-bit): hotplug presence detect.
    pub PresenceDetect [
        /// `AmdPresenceDetectSelectMode`.
        SELECT_MODE OFFSET(24) NUMBITS(2) []
    ],
    /// PCIEPORT `0x1A34_0014` (32-bit): the requester ID the port's own
    /// requests (MSIs) carry.
    pub TxRequester [
        FUNCTION OFFSET(0) NUMBITS(3) [],
        DEVICE OFFSET(3) NUMBITS(5) [],
        BUS OFFSET(8) NUMBITS(8) []
    ],
    /// PCIEPORT `0x1A34_0018` (32-bit): the segment of the requester in
    /// Flit mode.
    pub TxSegment [
        SEGMENT OFFSET(0) NUMBITS(8) [],
        VALID OFFSET(8) NUMBITS(1) []
    ],
    /// PCIEPORT `0x1A34_036C` (32-bit): the port's hotplug slot.
    pub HotplugSlot [
        /// The physical slot number.
        SLOT OFFSET(0) NUMBITS(13) [],
        VALID OFFSET(31) NUMBITS(1) []
    ],
    /// RC `0x1C40_0064` (32-bit): Link Capabilities of the root port.
    pub LinkCapabilities [
        MAX_SPEED OFFSET(0) NUMBITS(4) []
    ],
    /// RC `0x1C40_0068` (32-bit): Link Control (low half) and Link Status.
    pub LinkControlStatus [
        /// Link Control bit 12: set with SRIS (AMD-specific).
        SRIS OFFSET(12) NUMBITS(1) [],
        /// Link Status: Data Link Layer Link Active.
        DLL_ACTIVE OFFSET(29) NUMBITS(1) []
    ],
    /// RC `0x1C40_0080` (32-bit): Device Control 2.
    pub DeviceControl2 [
        COMPLETION_TIMEOUT OFFSET(0) NUMBITS(4) [],
        /// Bit 6, which the vendor ties to the port's `+0x89` bit 6.
        BIT6 OFFSET(6) NUMBITS(1) []
    ],
    /// RC `0x1C40_0088` (32-bit): Link Control 2.
    pub LinkControl2 [
        /// Bits 9:7 (Transmit Margin), from the port's `+0x28[6:4]`.
        TRANSMIT_MARGIN OFFSET(7) NUMBITS(3) [],
        /// Link Status 2 (the upper half) bit 10: Flit Mode Status.
        FLIT_MODE OFFSET(26) NUMBITS(1) []
    ],
    /// RC `0x1C40_004C` (32-bit): the root port's subsystem IDs.
    pub SubsystemId [
        VENDOR OFFSET(0) NUMBITS(16) [],
        DEVICE OFFSET(16) NUMBITS(16) []
    ],
    /// RC `0x1C40_055C` / `0x1C40_0560` (32-bit): NPEM capability / control.
    pub Npem [
        VALUE OFFSET(0) NUMBITS(12) []
    ],
    /// RC `0x1C44_0294` (32-bit): the port's LTSSM.
    pub Ltssm [
        STATE OFFSET(0) NUMBITS(6) []
    ],
    /// RC `0x1C44_02A8` (32-bit): the flit error counter.
    pub FlitErrorCounter [
        ENABLE OFFSET(0) NUMBITS(1) [],
        EVENTS OFFSET(2) NUMBITS(2) [],
        TRIGGER OFFSET(4) NUMBITS(8) []
    ],
    /// RC `0x1C44_0390`/`0394`/`0400`/`0480` (32-bit): Gen3/4/5/6
    /// equalization control.
    pub EqControl [
        /// Cleared after the reconfiguration (Gen6: `+0x28` bit 7).
        BIT0 OFFSET(0) NUMBITS(1) [],
        /// Cleared before it (descriptor `+0x23`, unnamed).
        BIT6 OFFSET(6) NUMBITS(1) [],
        FIXED_PRESET_EN OFFSET(10) NUMBITS(1) [],
        FIXED_PRESET OFFSET(11) NUMBITS(4) []
    ],
    /// RC `0x1C44_0800` (32-bit): flit mode drain rate.
    pub FlitDrain [
        COUNT OFFSET(0) NUMBITS(16) [],
        RATE OFFSET(17) NUMBITS(3) []
    ],
    /// RC `0x1C40_005A` (16-bit, read through a word proxy access): PCI
    /// Express Capabilities of the root port.
    pub PcieCapabilities [
        FLIT_MODE OFFSET(15) NUMBITS(1) []
    ],
    /// RC core `0x1C48_0060` (32-bit): the register lock the vendor sets at
    /// ready-to-boot.
    pub CoreLock [
        LOCK OFFSET(0) NUMBITS(1) []
    ],
    /// RC core `0x1C48_1020` (32-bit): bit 3 enables the flit drain rate.
    pub CoreFlitDrain [
        ENABLE OFFSET(3) NUMBITS(1) []
    ],
    /// RC `0x1C44_0424` (32-bit): CXL port status.
    pub CxlStatus [
        /// 3: the link negotiated the alternate (CXL) protocol.
        ALTERNATE_PROTOCOL OFFSET(12) NUMBITS(2) [],
        /// `PcdCxlTempGen5AdvertAltPtcl` (`0xF7DEB0`).
        TEMP_GEN5_ADVERT_ALT_PTCL OFFSET(29) NUMBITS(1) []
    ],
    /// RC `0x1C44_0280` (32-bit): the CXL port's link power settings at
    /// endpoint enable (`CxlConfigurePortFeatures`).
    pub CxlLink [
        /// Set to 9 (meaning not documented).
        BITS_15_12 OFFSET(12) NUMBITS(4) [],
        /// `!PcdAmdCxlAspm`.
        ASPM_DISABLE OFFSET(24) NUMBITS(1) []
    ],
    /// RC `0x1C44_0444` (32-bit): CXL.io and CXL.cache/mem L1/L2 disables
    /// (`!PcdAmdCxlIoL1`, `!...CaMemL1`, `!...IoL2`, `!...CaMemL2`).
    pub CxlPowerStates [
        IO_L1_DISABLE OFFSET(6) NUMBITS(1) [],
        CAMEM_L1_DISABLE OFFSET(7) NUMBITS(1) [],
        IO_L2_DISABLE OFFSET(8) NUMBITS(1) [],
        CAMEM_L2_DISABLE OFFSET(9) NUMBITS(1) []
    ],
    /// RC `0x1C44_046C` (32-bit): bit 5 on CXL ports before the
    /// reconfiguration (`0xF7DEB0`), bit 27 after it.
    pub Lc46c [
        CXL OFFSET(5) NUMBITS(1) [],
        BIT27 OFFSET(27) NUMBITS(1) []
    ],
    /// RC `0x1C44_0784` (32-bit): CXL NOP insertion (`PcdAmdCxlNopInsertion`).
    pub CxlNop [
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// 1 for insertion mode 1, 2 for mode 3, else 0.
        MODE OFFSET(3) NUMBITS(8) []
    ],
    /// PCIEPORT-aperture `0x1A38_031C` (32-bit): CXL.mem isolation.
    pub CxlIsolation [
        /// `!PcdAmdCxlMemIsolationEnable`.
        DISABLE OFFSET(10) NUMBITS(1) []
    ],
    /// RC `0x1C44_0440` (32-bit): CXL arbitration weights.
    pub CxlArbitration [
        IO_WEIGHTS OFFSET(16) NUMBITS(4) [],
        CA_MEM_WEIGHTS OFFSET(20) NUMBITS(4) [],
        /// Use the weights above.
        OVERRIDE OFFSET(24) NUMBITS(1) []
    ],
    /// RC `0x1C44_00BC` (32-bit): CNLI token advertisement.
    pub CnliToken [
        TOKENS OFFSET(10) NUMBITS(5) [],
        OVERRIDE OFFSET(15) NUMBITS(1) []
    ],
    /// RC `0x1C44_04D0` (32-bit): CXL.mem response in power management
    /// (bits 9 and 11; also bit 17, which the after-reconfiguration step
    /// clears). Meanings not documented.
    pub CxlPm [
        RESPONSE_A OFFSET(9) NUMBITS(1) [],
        RESPONSE_B OFFSET(11) NUMBITS(1) [],
        BIT17 OFFSET(17) NUMBITS(1) []
    ],
    /// RC `0x1C44_045C` (32-bit): CXL transmit path.
    pub CxlTx [
        /// Cleared on every CXL port (meaning not documented).
        BIT0 OFFSET(0) NUMBITS(1) [],
        DIRECT_OUT OFFSET(1) NUMBITS(1) [],
        /// Set at endpoint enable unless the link runs in flit mode on a
        /// part other than A0 (`CxlConfigurePortFeatures`).
        BIT5 OFFSET(5) NUMBITS(1) []
    ],
    /// RC core `0x1C48_04E4` (32-bit): CXL.cache/mem receive.
    pub CxlCamem [
        RX_OPTIMIZATION OFFSET(2) NUMBITS(1) []
    ],
    /// CNLI `0x1F10_0090` (32-bit): poison masks (Turin `0x1F10_0070`).
    pub CnliPoisonA [
        LOW OFFSET(0) NUMBITS(3) [],
        MASK OFFSET(5) NUMBITS(1) []
    ],
    /// CNLI `0x1F10_00E8` (32-bit): the root port's CXL Timeout and
    /// Isolation Capability register (CXL 3.1 §8.2.4.24.1; AmdNbioDxe
    /// `0x2AE0` reads it through host SMN, at `0x1F10_20E8`, see
    /// [`TIMEOUT_ISOLATION_CAPABILITY`]).
    pub TimeoutIsolationCapability [
        /// CXL.mem Transaction Timeout Ranges Supported (the PCIe
        /// Completion Timeout ranges A..D, one bit each).
        MEM_TIMEOUT_RANGES OFFSET(0) NUMBITS(4) [],
        MEM_TIMEOUT_SUPPORTED OFFSET(4) NUMBITS(1) [],
        MEM_ISOLATION_SUPPORTED OFFSET(16) NUMBITS(1) [],
        MEM_ISOLATION_LINK_DOWN_SUPPORTED OFFSET(17) NUMBITS(1) []
    ],
    /// CNLI `0x1F10_00F0` (32-bit): the root port's CXL Timeout and
    /// Isolation Control register (CXL 3.1 §8.2.4.24.2).
    pub TimeoutIsolationControl [
        /// CXL.mem Transaction Timeout Value (PCIe Completion Timeout
        /// encoding; 0 the default range).
        MEM_TIMEOUT_VALUE OFFSET(0) NUMBITS(4) [],
        MEM_TIMEOUT_ENABLE OFFSET(4) NUMBITS(1) [],
        MEM_ISOLATION_ENABLE OFFSET(16) NUMBITS(1) [],
        MEM_ISOLATION_LINK_DOWN_ENABLE OFFSET(17) NUMBITS(1) [],
        /// Isolation Interrupt Enable (`PcdAmdCxlMemIsolationFwNotification`).
        ISOLATION_INTERRUPT_ENABLE OFFSET(25) NUMBITS(1) []
    ],
    /// PCIEPORT `0x1A34_0264` (32-bit): how the port signals CXL.mem
    /// isolation (AmdNbioDxe `0x2AE0`; names not known).
    pub IsolationSignal [
        /// 2 with firmware notification of isolation.
        NOTIFY OFFSET(2) NUMBITS(2) [],
        /// Set when the port does not isolate on link down.
        NO_LINK_DOWN OFFSET(4) NUMBITS(1) []
    ],
    /// CNLI `0x1F10_0084` (32-bit): poison masks (Turin `0x1F10_0064`).
    pub CnliPoisonB [
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT4 OFFSET(4) NUMBITS(1) [],
        MASK OFFSET(10) NUMBITS(1) []
    ],
    /// PCIEPORT `0x1A34_091C` (32-bit): CXL QoS limits.
    pub QosLimits [
        HIGH OFFSET(0) NUMBITS(16) [],
        NORMAL OFFSET(16) NUMBITS(16) []
    ],
    /// PCIEPORT `0x1A34_0920` (32-bit): CXL QoS timer.
    pub QosTimer [
        SCHED_GAP OFFSET(0) NUMBITS(12) [],
        TIMER_LIMIT OFFSET(16) NUMBITS(12) []
    ],
    /// PCIEPORT `0x1A34_0924` (32-bit): CXL QoS mode.
    pub QosMode [
        WRRSP_INC_NUM OFFSET(0) NUMBITS(4) [],
        RDSP_INC_NUM OFFSET(4) NUMBITS(4) [],
        TIMER_DEC_NUM OFFSET(8) NUMBITS(4) [],
        RDSP_INC_MODE OFFSET(12) NUMBITS(1) [],
        VARIABLE_GAP OFFSET(13) NUMBITS(1) []
    ]
];

/// The root port's own PCI configuration registers the steps write
/// through ECAM (its PCI Express capability is at `0x58`; the `0x1C40`
/// aperture mirrors this space).
pub mod config {
    use tock_registers::register_bitfields;

    /// The PCI Express capability.
    pub const PCIE_CAP: u16 = 0x58;
    /// PCI Express Capabilities (16-bit): [`fstart_pci::PCI_EXPRESS_CAPABILITIES`].
    pub const PCIE_CAPABILITIES: u16 = PCIE_CAP + 0x02;
    /// Device Control (16-bit): [`DeviceControl`].
    pub const DEVICE_CONTROL: u16 = PCIE_CAP + 0x08;
    /// Slot Capabilities (32-bit): [`SlotCapabilities`].
    pub const SLOT_CAPABILITIES: u16 = PCIE_CAP + 0x14;
    /// Slot Control (16-bit): [`SlotControl`].
    pub const SLOT_CONTROL: u16 = PCIE_CAP + 0x18;
    /// Slot Capabilities 2 (32-bit): bit 0 In-Band PD Disable Supported.
    pub const SLOT_CAPABILITIES2: u16 = PCIE_CAP + 0x34;
    /// Extended configuration `0x54C` (16-bit): bits 4 and 2 set on an SFI
    /// hotplug port with an endpoint (unnamed).
    pub const SFI_54C: u16 = 0x54c;

    register_bitfields![u32,
        /// Slot Capabilities.
        pub SlotCapabilities [
            /// Bits 5:0 and 18:17 are the hotplug type's (the vendor's
            /// `0x2003A`/`0x2001B`/`0x40020` patterns).
            HOTPLUG_CAPABLE OFFSET(6) NUMBITS(1) [],
            POWER_LIMIT OFFSET(7) NUMBITS(8) [],
            POWER_SCALE OFFSET(15) NUMBITS(2) [],
            PHYSICAL_SLOT OFFSET(19) NUMBITS(13) []
        ]
    ];
    register_bitfields![u16,
        /// Device Control.
        pub DeviceControl [
            MAX_PAYLOAD OFFSET(5) NUMBITS(3) []
        ],
        /// Slot Control.
        pub SlotControl [
            PRESENCE_DETECT_CHANGED_EN OFFSET(3) NUMBITS(1) [],
            /// Bit 14 (Auto Slot Power Limit Disable), `SilData+0x17`.
            BIT14 OFFSET(14) NUMBITS(1) []
        ]
    ];
}

use Aperture::{Cnli, Core, Port, RcCore, RcPort};

// PCIECORE.
pub const STRAP_F0: Reg<PcieStrapF0::Register> = Reg::proxy(0x1a38_02c0, Core);
/// Hotplug: bits 24:23 set on a hotplug core.
pub const CORE_100: Reg = Reg::proxy(0x1a38_0100, Core);
/// Hotplug: one bit per port, cleared for a hotplug port.
pub const CORE_438: Reg = Reg::proxy(0x1a38_0438, Core);
pub const PRESENCE_DETECT: Reg<PresenceDetect::Register> = Reg::proxy(0x1a38_04e0, Core);
pub const CORE_008: Reg<Core008::Register> = Reg::proxy(0x1a38_0008, Core);
pub const CORE_024: Reg<Core024::Register> = Reg::proxy(0x1a38_0024, Core);
pub const CORE_204: Reg<Core204::Register> = Reg::proxy(0x1a38_0204, Core);
pub const CORE_304: Reg<Core304::Register> = Reg::proxy(0x1a38_0304, Core);
pub const CXL_ERROR_LOG: Reg<CxlErrorLog::Register> = Reg::proxy(0x1a38_0314, Core);
pub const CORE_70C: Reg<Core70c::Register> = Reg::proxy(0x1a38_070c, Core);
pub const CORE_854: Reg<Core854::Register> = Reg::proxy(0x1a38_0854, Core);

// PCIEPORT.
pub const TX_REQUESTER: Reg<TxRequester::Register> = Reg::proxy(0x1a34_0014, Port);
pub const TX_SEGMENT: Reg<TxSegment::Register> = Reg::proxy(0x1a34_0018, Port);
/// Hotplug: bit 4.
pub const PORT_040: Reg = Reg::proxy(0x1a34_0040, Port);
/// Hotplug: bits 31:24 = `0x20`.
pub const PORT_1A0: Reg = Reg::proxy(0x1a34_01a0, Port);
/// `PcieHotplugPreInitWh`: bits 1:0 cleared.
pub const PORT_348: Reg = Reg::proxy(0x1a34_0348, Port);
/// Hotplug presence/SFI settings.
pub const PORT_368: Reg = Reg::proxy(0x1a34_0368, Port);
pub const HOTPLUG_SLOT: Reg<HotplugSlot::Register> = Reg::proxy(0x1a34_036c, Port);
/// Bits 15 and 20 (`SilData+0x5D`).
pub const PORT_608: Reg = Reg::proxy(0x1a34_0608, Port);
/// Bit 0, from the port's `+0x89` bit 6.
pub const PORT_60C: Reg = Reg::proxy(0x1a34_060c, Port);
/// `MST_PORT_CTRL_2` (SFI): `0x1A34_06C4`, on SP7 A0 `0x1A34_0684`.
pub const MST_PORT_CTRL_2: Reg = Reg::proxy(0x1a34_06c4, Port);
pub const MST_PORT_CTRL_2_A0: Reg = Reg::proxy(0x1a34_0684, Port);
/// Bit 2, from the port's `+0x89` bit 6.
pub const PORT_904: Reg = Reg::proxy(0x1a34_0904, Port);
/// Bonus core: `0x22 + port` after the bifurcation (`0xf82a40`).
pub const PORT_90C: Reg = Reg::proxy(0x1a34_090c, Port);
pub const QOS_LIMITS: Reg<QosLimits::Register> = Reg::proxy(0x1a34_091c, Port);
pub const QOS_TIMER: Reg<QosTimer::Register> = Reg::proxy(0x1a34_0920, Port);
pub const QOS_MODE: Reg<QosMode::Register> = Reg::proxy(0x1a34_0924, Port);

// RC port: the root port's configuration space mirror.
pub const SUBSYSTEM_ID: Reg<SubsystemId::Register> = Reg::proxy(0x1c40_004c, RcPort);
/// 16-bit, read with [`Width::Word`].
pub const PCIE_CAPABILITIES: Reg<PcieCapabilities::Register> = Reg::proxy(0x1c40_005a, RcPort);
/// Read through SMN by the vendor ([module documentation](self)).
pub const LINK_CAPABILITIES: Reg<LinkCapabilities::Register> = Reg::proxy(0x1c40_0064, RcPort);
/// Read through SMN, written through the proxy by the vendor.
pub const LINK_CONTROL_STATUS: Reg<LinkControlStatus::Register> = Reg::proxy(0x1c40_0068, RcPort);
pub const DEVICE_CONTROL2: Reg<DeviceControl2::Register> = Reg::proxy(0x1c40_0080, RcPort);
pub const LINK_CONTROL2: Reg<LinkControl2::Register> = Reg::proxy(0x1c40_0088, RcPort);
pub const NPEM_CAPABILITY: Reg<Npem::Register> = Reg::new(0x1c40_055c, RcPort, Access::Smn);
pub const NPEM_CONTROL: Reg<Npem::Register> = Reg::new(0x1c40_0560, RcPort, Access::Smn);

// RC port: link controller (LC) registers. The unnamed ones by offset.
pub const CNLI_TOKEN: Reg<CnliToken::Register> = Reg::proxy(0x1c44_00bc, RcPort);
pub const LC_280: Reg<CxlLink::Register> = Reg::proxy(0x1c44_0280, RcPort);
pub const LC_284: Reg = Reg::proxy(0x1c44_0284, RcPort);
/// Bit 30 turns off unused lanes; bits 10, 17, 19 after training.
pub const LC_288: Reg = Reg::proxy(0x1c44_0288, RcPort);
pub const LTSSM: Reg<Ltssm::Register> = Reg::proxy(0x1c44_0294, RcPort);
pub const FLIT_ERROR_COUNTER: Reg<FlitErrorCounter::Register> =
    Reg::new(0x1c44_02a8, RcPort, Access::Smn);
pub const LC_2C4: Reg = Reg::proxy(0x1c44_02c4, RcPort);
pub const LC_2D4: Reg = Reg::proxy(0x1c44_02d4, RcPort);
pub const LC_2D8: Reg = Reg::proxy(0x1c44_02d8, RcPort);
pub const LC_300: Reg = Reg::proxy(0x1c44_0300, RcPort);
pub const LC_32C: Reg = Reg::proxy(0x1c44_032c, RcPort);
/// Gen3, Gen4, Gen5 and Gen6 equalization control.
pub const EQ_CONTROL: [Reg<EqControl::Register>; 4] = [
    Reg::proxy(0x1c44_0390, RcPort),
    Reg::proxy(0x1c44_0394, RcPort),
    Reg::proxy(0x1c44_0400, RcPort),
    Reg::proxy(0x1c44_0480, RcPort),
];
/// Bits 30:28 from the port's `+0x81[6:4]`.
pub const LC_408: Reg = Reg::proxy(0x1c44_0408, RcPort);
/// Gen3, Gen4 and Gen5 fixed transmitter coefficients.
pub const EQ_COEFFICIENTS: [Reg<EqCoefficients::Register>; 3] = [
    Reg::proxy(0x1c44_02e0, RcPort),
    Reg::proxy(0x1c44_037c, RcPort),
    Reg::proxy(0x1c44_0418, RcPort),
];
/// Bits 23:19: compliance (`AmdAllowCompliance`).
pub const LC_40C: Reg = Reg::proxy(0x1c44_040c, RcPort);
pub const LC_414: Reg = Reg::proxy(0x1c44_0414, RcPort);
pub const CXL_STATUS: Reg<CxlStatus::Register> = Reg::proxy(0x1c44_0424, RcPort);
pub const CXL_ARBITRATION: Reg<CxlArbitration::Register> = Reg::proxy(0x1c44_0440, RcPort);
pub const CXL_POWER_STATES: Reg<CxlPowerStates::Register> = Reg::proxy(0x1c44_0444, RcPort);
pub const CXL_TX: Reg<CxlTx::Register> = Reg::proxy(0x1c44_045c, RcPort);
pub const LC_46C: Reg<Lc46c::Register> = Reg::proxy(0x1c44_046c, RcPort);
pub const LC_47C: Reg = Reg::proxy(0x1c44_047c, RcPort);
pub const CXL_PM: Reg<CxlPm::Register> = Reg::proxy(0x1c44_04d0, RcPort);
pub const FLIT_DRAIN: Reg<FlitDrain::Register> = Reg::proxy(0x1c44_0800, RcPort);
pub const CXL_NOP: Reg<CxlNop::Register> = Reg::proxy(0x1c44_0784, RcPort);
pub const LC_A54: Reg = Reg::proxy(0x1c44_0a54, RcPort);
/// The CXL.mem isolation control, per port in the PCIEPORT aperture.
pub const CXL_ISOLATION: Reg<CxlIsolation::Register> = Reg::proxy(0x1a38_031c, Port);

// RC core.
pub const CORE_LOCK: Reg<CoreLock::Register> = Reg::proxy(0x1c48_0060, RcCore);
/// A free-running counter the vendor's hotplug setup waits on
/// ([`crate::dxio::wh::hotplug::settle`]).
pub const CORE_COUNTER: Reg = Reg::proxy(0x1c48_0488, RcCore);
pub const CXL_CAMEM: Reg<CxlCamem::Register> = Reg::proxy(0x1c48_04e4, RcCore);
pub const CORE_FLIT_DRAIN: Reg<CoreFlitDrain::Register> = Reg::proxy(0x1c48_1020, RcCore);

// CNLI (host SMN).
pub const CNLI_POISON_A: Reg<CnliPoisonA::Register> = Reg::new(0x1f10_0090, Cnli, Access::Smn);
pub const CNLI_POISON_B: Reg<CnliPoisonB::Register> = Reg::new(0x1f10_0084, Cnli, Access::Smn);
/// The Timeout and Isolation Capability the CXL spec places 8 bytes below
/// its control ([`TIMEOUT_ISOLATION_CONTROL`]). The vendor reads
/// `0x1F10_20E8` instead, which with the CNLI port stride of `0x2000` is
/// the next port's (a donor slip, or a register whose name is not known);
/// fstart reads the port's own.
pub const TIMEOUT_ISOLATION_CAPABILITY: Reg<TimeoutIsolationCapability::Register> =
    Reg::new(0x1f10_00e8, Cnli, Access::Smn);
pub const TIMEOUT_ISOLATION_CONTROL: Reg<TimeoutIsolationControl::Register> =
    Reg::new(0x1f10_00f0, Cnli, Access::Smn);
pub const ISOLATION_SIGNAL: Reg<IsolationSignal::Register> = Reg::proxy(0x1a34_0264, Port);

/// A trained root port's requester ID (`AfterDxioInitPerPortProgramming`,
/// SilVenicePei `0xf83670`): `bus << 8 | devfn` at `0x1A34_0014`, and when
/// the port advertises Flit mode, `segment | 0x100` at `0x1A34_0018`.
pub fn set_requester<A: SmnAccess>(
    proxy: &Proxy<'_, A>,
    port: PortId,
    (segment, bus, devfn): (u8, u8, u8),
) -> Result<(), Error> {
    proxy.reg(TX_REQUESTER.at(port)).modify(
        TxRequester::BUS.val(bus.into())
            + TxRequester::DEVICE.val(u32::from(devfn >> 3))
            + TxRequester::FUNCTION.val(u32::from(devfn & 7)),
    )?;
    let caps = proxy.read(PCIE_CAPABILITIES.address(port), Width::Word)?;
    if LocalRegisterCopy::<u32, PcieCapabilities::Register>::new(caps)
        .is_set(PcieCapabilities::FLIT_MODE)
    {
        proxy
            .reg(TX_SEGMENT.at(port))
            .modify(TxSegment::SEGMENT.val(segment.into()) + TxSegment::VALID::SET)?;
    }
    Ok(())
}

/// The ready-to-boot lock of the PCIe core behind RC aperture `rc`.
pub fn lock_core<A: SmnAccess>(proxy: &Proxy<'_, A>, rc: u8) -> Result<(), Error> {
    proxy
        .reg(CORE_LOCK.of_core(0, rc))
        .modify(CoreLock::LOCK::SET)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apertures_place_every_instance() {
        let p = PortId {
            core: 2,
            rc: 3,
            port: 1,
        };
        assert_eq!(
            TX_REQUESTER.at(p).address(),
            0x1a34_0014 + 0x20_0000 + 0x1000
        );
        assert_eq!(STRAP_F0.at(p).address(), 0x1a38_02c0 + 0x20_0000);
        assert_eq!(LC_288.at(p).address(), 0x1c44_0288 + 0x30_0000 + 0x1000);
        assert_eq!(CORE_LOCK.at(p).address(), 0x1c48_0060 + 0x30_0000);
        assert_eq!(
            CNLI_POISON_A.at(PortId { core: 6, ..p }).address(),
            0x1f10_0090 + (2 << 16) + (1 << 13)
        );
        assert_eq!(FLIT_ERROR_COUNTER.access(), Access::Smn);
        assert_eq!(LC_288.access(), Access::Proxy);
    }
}
