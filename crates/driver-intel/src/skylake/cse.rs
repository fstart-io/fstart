// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native SKL/KBL CSE services, not a memory initializer or map.
mod post_memory;
use super::sa_registers::{MESEG_BASE, MESEG_MASK, MeSegmentMask};
use crate::heci::{self, Budgets, Csr, Fifo, MmioTransport, Transport};
use embedded_hal::delay::DelayNs;
use fstart_pci::EcamDevice;
pub use post_memory::{BiosPayload, EndOfPostAction, MBP_BUFFER_BYTES};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{U16, U32},
};

register_bitfields![u32,
    pub Hfs1 [
        WORKING OFFSET(0) NUMBITS(4) [],
        MANUFACTURING OFFSET(4) NUMBITS(1) [],
        FPT_BAD OFFSET(5) NUMBITS(1) [],
        OPSTATE OFFSET(6) NUMBITS(3) [],
        INIT_COMPLETE OFFSET(9) NUMBITS(1) [],
        UPDATE OFFSET(11) NUMBITS(1) [],
        ERROR OFFSET(12) NUMBITS(4) [],
        MODE OFFSET(16) NUMBITS(4) []
    ],
    pub Hfs2 [
        CPU_REPLACED OFFSET(4) NUMBITS(1) [],
        WARM_RESET_DF OFFSET(7) NUMBITS(1) [],
        CPU_VALID OFFSET(8) NUMBITS(1) []
    ],
    pub Hfs4 [ ENFORCEMENT OFFSET(9) NUMBITS(1) [] ],
    pub MiscShadow [
        SIZE_MIB OFFSET(0) NUMBITS(7) [],
        VALID OFFSET(16) NUMBITS(1) []
    ],
    pub DramDone [
        BASE_MIB OFFSET(0) NUMBITS(24) [],
        STATUS OFFSET(24) NUMBITS(4) [],
        TYPE OFFSET(28) NUMBITS(4) []
    ],
    pub IccData [ WARM_RESET OFFSET(0) NUMBITS(1) [] ],
    pub Bar [
        IO OFFSET(0) NUMBITS(1) [],
        TYPE OFFSET(1) NUMBITS(2) [],
        PREFETCH OFFSET(3) NUMBITS(1) [],
        ADDRESS OFFSET(4) NUMBITS(28) []
    ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Transport(heci::Error),
    WrongDevice,
    Unavailable,
    Firmware,
    Unsupported,
    InvalidBar,
    InvalidUma,
    InvalidBudget,
    Timeout,
    ResponseSize { expected: usize, actual: usize },
    ResponseHeader,
    FirmwareResult(u8),
    IccResult(u32),
    InvalidHsioBuffer,
    InvalidAction(u8),
    InvalidEopAction(u32),
    InvalidBiosPayload,
    BclkSkipped(BclkSkip),
}
impl From<heci::Error> for Error {
    fn from(e: heci::Error) -> Self {
        Self::Transport(e)
    }
}

/// Exact priority ordering in skylake/me_policy.c (SPS has highest rank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BiosPath {
    Normal,
    FirmwareUpdate,
    Error,
    Recovery,
    ErrorWithoutDid,
    SecurityOverrideMei,
    TemporaryDisable,
    SecurityOverrideJumper,
    Debug,
    Enforcement,
    Sps,
}
#[derive(Debug, Clone, Copy)]
pub struct FirmwareStatus {
    pub hfs1: u32,
    pub hfs4: u32,
}
impl FirmwareStatus {
    pub fn new(hfs1: u32, hfs4: u32) -> Result<Self, Error> {
        if hfs1 == 0 || hfs1 == u32::MAX || hfs4 == u32::MAX {
            return Err(Error::Unavailable);
        }
        Ok(Self { hfs1, hfs4 })
    }
    pub fn read(device: &EcamDevice) -> Result<Self, Error> {
        validate_device(device)?;
        Self::new(device.read32(0x40), device.read32(0x64))
    }
    pub fn bios_path(self) -> BiosPath {
        let h = LocalRegisterCopy::<u32, Hfs1::Register>::new(self.hfs1);
        let mut path = BiosPath::Normal;
        let error = h.read(Hfs1::ERROR);
        let mode = h.read(Hfs1::MODE);
        if error == 0 {
            if mode == 15 {
                path = BiosPath::Sps;
            } else if h.read(Hfs1::WORKING) == 5 && h.is_set(Hfs1::UPDATE) {
                path = BiosPath::FirmwareUpdate;
            }
        }
        if error == 3 {
            path = path.max(BiosPath::ErrorWithoutDid);
        } else if error != 0 {
            path = path.max(BiosPath::Error);
        }
        if h.is_set(Hfs1::FPT_BAD) {
            path = path.max(BiosPath::Error);
        }
        if h.read(Hfs1::WORKING) == 2 {
            path = path.max(BiosPath::Recovery);
        }
        if self.hfs4 & Hfs4::ENFORCEMENT::SET.value != 0 {
            path = path.max(BiosPath::Enforcement);
        }
        match mode {
            2 => path.max(BiosPath::Debug),
            3 => path.max(BiosPath::TemporaryDisable),
            4 => path.max(BiosPath::SecurityOverrideJumper),
            5 => path.max(BiosPath::SecurityOverrideMei),
            _ => path,
        }
    }
    /// Restricts the source filter to the supported native commands.
    pub fn allows(self, command: Command) -> bool {
        use BiosPath::*;
        let path = self.bios_path();
        // Native me_policy.c permits EOP on every still-accessible path.
        if command == Command::EndOfPost {
            return true;
        }
        if matches!(path, Normal | FirmwareUpdate | Sps) {
            return true;
        }
        command == Command::DramInitDone
            && matches!(
                path,
                Error | Recovery | SecurityOverrideMei | TemporaryDisable | SecurityOverrideJumper
            )
    }
    pub fn is_debug_mode(self) -> bool {
        LocalRegisterCopy::<u32, Hfs1::Register>::new(self.hfs1).read(Hfs1::MODE) == 2
    }
    fn initialization_allowed(self) -> bool {
        self.hfs1 & Hfs1::FPT_BAD::SET.value == 0 && !self.is_debug_mode()
    }
    pub fn initialized_without_uma(self) -> bool {
        let h = LocalRegisterCopy::<u32, Hfs1::Register>::new(self.hfs1);
        h.is_set(Hfs1::INIT_COMPLETE) && h.read(Hfs1::OPSTATE) == 5
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    DramInitDone,
    Bclk,
    HsioCrc,
    BiosPayload,
    FirmwareCapabilities,
    EndOfPost,
}
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct MkhiHeader {
    group: u8,
    opcode: u8,
    reserved: u8,
    result: u8,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct MkhiU32Message {
    header: MkhiHeader,
    data: U32,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct HsioCrcData {
    crc: U16,
    version: u8,
    family: u8,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct IccHeader {
    api: U32,
    command: U32,
    result: U32,
    length: U32,
    reserved: U32,
}

#[repr(C)]
#[derive(Clone, Copy, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct IccSetHsioHeader {
    icc: IccHeader,
    flags: U32,
}

impl Command {
    fn group(self) -> u8 {
        match self {
            Self::DramInitDone | Self::BiosPayload => 0xf0,
            Self::Bclk => 0xf1,
            Self::HsioCrc => 0xf2,
            Self::FirmwareCapabilities => 3,
            Self::EndOfPost => 0xff,
        }
    }
    fn opcode(self) -> u8 {
        match self {
            Self::BiosPayload | Self::FirmwareCapabilities => 2,
            Self::EndOfPost => 0x0c,
            _ => 1,
        }
    }
    fn header(self) -> MkhiHeader {
        MkhiHeader {
            group: self.group(),
            opcode: self.opcode(),
            reserved: 0,
            result: 0,
        }
    }
    fn request(self, data: u32) -> MkhiU32Message {
        MkhiU32Message {
            header: self.header(),
            data: U32::new(data),
        }
    }
}

fn validate_device(device: &EcamDevice) -> Result<(), Error> {
    let a = device.address();
    if a.segment() != 0 || a.bus() != 0 || a.device() != 0x16 || a.function() != 0 {
        return Err(Error::WrongDevice);
    }
    if device.try_read16(0) != Some(0x8086)
        || matches!(device.try_read16(2), None | Some(0) | Some(0xffff))
    {
        return Err(Error::Unavailable);
    }
    Ok(())
}

/// Validates PCI BAR attributes without probing/sizing a live device. Source
/// pre-memory assignment explicitly clears BAR1, so this binding is below 4 GiB.
pub fn decoded_bar(device: &EcamDevice) -> Result<usize, Error> {
    validate_device(device)?;
    let raw = device.read32(0x10);
    let bar = LocalRegisterCopy::<u32, Bar::Register>::new(raw);
    if bar.is_set(Bar::IO) || bar.is_set(Bar::PREFETCH) || !matches!(bar.read(Bar::TYPE), 0 | 2) {
        return Err(Error::InvalidBar);
    }
    if bar.read(Bar::TYPE) == 2 && device.read32(0x14) != 0 {
        return Err(Error::InvalidBar);
    }
    let base = (bar.read(Bar::ADDRESS) << 4) as usize;
    if base == 0 || base & 0xfff != 0 || base.checked_add(0xfff).is_none() || base > 0xffff_f000 {
        return Err(Error::InvalidBar);
    }
    Ok(base)
}

/// The fixed HECI1 BAR native romstage uses (HECI1_BASE_ADDRESS) and the
/// cold owner keeps for the whole boot.
pub const HECI1_BASE: u32 = 0xfed1_a000;

/// Pre-memory PCI assignment only. No interface reset. Preserve command word
/// neighbours and BAR attributes; never relocate an already-assigned resource.
/// # Safety
/// Own this visible HECI1 and the reserved, mapped 4 KiB `base` exclusively;
/// ECAM and the chipset identity must already be established by platform code.
pub unsafe fn assign_bar(device: &EcamDevice, base: u32) -> Result<(), Error> {
    validate_device(device)?;
    if base == 0 || base & 0xfff != 0 || base > 0xffff_f000 {
        return Err(Error::InvalidBar);
    }
    let status = FirmwareStatus::read(device)?;
    if !status.initialization_allowed() && !status.is_debug_mode() {
        return Err(Error::Firmware);
    }
    let raw = device.read32(0x10);
    let attrs = raw & 0xf;
    if !matches!(attrs, 0 | 4) || (attrs == 4 && device.read32(0x14) != 0) {
        return Err(Error::InvalidBar);
    }
    let command = device.read16(4);
    if raw & !0xf != 0 {
        if decoded_bar(device)? != base as usize {
            return Err(Error::InvalidBar);
        }
    } else {
        device.write16(4, command & !6);
        device.write32(0x10, base | attrs);
        if attrs == 4 {
            device.write32(0x14, 0);
        }
    }
    device.write16(4, command | 6);
    Ok(())
}

/// A firmware-reported size; unavailable/timeouts are errors, not UMA=0.
/// ME Debug mode explicitly skips UMA allocation (0 MiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UmaRequest {
    mib: u8,
}
impl UmaRequest {
    pub const fn zero() -> Self {
        Self { mib: 0 }
    }
    pub fn from_shadow(raw: u32) -> Result<Option<Self>, Error> {
        if raw == u32::MAX {
            return Err(Error::Unavailable);
        }
        let shadow = LocalRegisterCopy::<u32, MiscShadow::Register>::new(raw);
        if !shadow.is_set(MiscShadow::VALID) {
            return Ok(None);
        }
        let mib = shadow.read(MiscShadow::SIZE_MIB) as u8;
        if mib > 0x40 {
            return Err(Error::InvalidUma);
        }
        Ok(Some(Self { mib }))
    }
    pub fn mib(self) -> u8 {
        self.mib
    }
}
/// Bounded source MISC_SHDW wait, 1 ms intervals, at most 5 s.
pub fn wait_uma(
    device: &EcamDevice,
    delays_ms: u32,
    delay: &mut impl DelayNs,
) -> Result<UmaRequest, Error> {
    if delays_ms == 0 || delays_ms > 5000 {
        return Err(Error::InvalidBudget);
    }
    for sample in 0..=delays_ms {
        let status = FirmwareStatus::read(device)?;
        if status.is_debug_mode() {
            fstart_log::warn!("CSE: ME in Debug mode; skipping UMA wait (UMA=0)");
            return Ok(UmaRequest::zero());
        }
        if let Some(request) = UmaRequest::from_shadow(device.read32(0x44))? {
            return Ok(request);
        }
        if sample != delays_ms {
            delay.delay_ms(1);
        }
    }
    Err(Error::Timeout)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuReplacement {
    pub replaced: bool,
    pub dynamic_fusing_warm_reset: bool,
}
impl CpuReplacement {
    pub fn from_hfs2(raw: u32) -> Result<Option<Self>, Error> {
        if raw == u32::MAX {
            return Err(Error::Unavailable);
        }
        let h = LocalRegisterCopy::<u32, Hfs2::Register>::new(raw);
        if !h.is_set(Hfs2::CPU_VALID) {
            return Ok(None);
        }
        Ok(Some(Self {
            replaced: h.is_set(Hfs2::CPU_REPLACED),
            dynamic_fusing_warm_reset: h.is_set(Hfs2::WARM_RESET_DF),
        }))
    }
}
pub fn wait_cpu_replacement(
    device: &EcamDevice,
    delays_ms: u32,
    delay: &mut impl DelayNs,
) -> Result<CpuReplacement, Error> {
    if delays_ms == 0 || delays_ms > 50 {
        return Err(Error::InvalidBudget);
    }
    for sample in 0..=delays_ms {
        FirmwareStatus::read(device)?;
        if let Some(status) = CpuReplacement::from_hfs2(device.read32(0x48))? {
            return Ok(status);
        }
        if sample != delays_ms {
            delay.delay_ms(1);
        }
    }
    Err(Error::Timeout)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DramInitStatus {
    Success = 0,
    NoMemory = 1,
    MemoryNotPreserved = 3,
}
/// Caller assertion, not a DRAM/map certificate. Constructor is unsafe because
/// register encoding checks cannot prove RAM usability, reservation, or DMA safety.
#[derive(Debug, Clone, Copy)]
pub struct ValidatedDram {
    request: UmaRequest,
    base: u64,
    status: DramInitStatus,
}
impl ValidatedDram {
    /// # Safety
    /// Caller genuinely validated DRAM status and the programmed MESEG base
    /// against its reserved UMA map, using this firmware-reported request.
    /// Success requires working DRAM; this API neither tests nor allocates it.
    pub unsafe fn new(
        request: UmaRequest,
        base: u64,
        status: DramInitStatus,
    ) -> Result<Self, Error> {
        let size = u64::from(request.mib) << 20;
        // Intel 332688-003EN pp65–66: an allocated ME range must have
        // contiguous mask bits and natural alignment. A no-memory/base-zero
        // report allocates nothing and can truthfully reject a bad request.
        let invalid_allocation = request.mib != 0
            && base != 0
            && (!request.mib.is_power_of_two() || base & (size - 1) != 0);
        if invalid_allocation
            || base & 0xfffff != 0
            || base >= (1u64 << 36)
            || (request.mib == 0 && base != 0)
            || (request.mib != 0 && base == 0 && status != DramInitStatus::NoMemory)
            || base.checked_add(size).is_none_or(|end| end > (1u64 << 36))
        {
            return Err(Error::InvalidUma);
        }
        Ok(Self {
            request,
            base,
            status,
        })
    }
    pub(crate) fn validate_me_segment(self, sa: &EcamDevice) -> Result<(), Error> {
        let base = u64::from(sa.read32(MESEG_BASE)) | (u64::from(sa.read32(MESEG_BASE + 4)) << 32);
        let mask = LocalRegisterCopy::<u64, MeSegmentMask::Register>::new(
            u64::from(sa.read32(MESEG_MASK)) | (u64::from(sa.read32(MESEG_MASK + 4)) << 32),
        );
        if base != self.base {
            return Err(Error::InvalidUma);
        }
        if self.request.mib == 0 || self.base == 0 && self.status == DramInitStatus::NoMemory {
            // A genuine zero request, or a failed-memory report with no UMA,
            // must not retain an enabled reservation. No RAM is certified here.
            if mask.is_set(MeSegmentMask::STOLEN_ENABLE) {
                return Err(Error::InvalidUma);
            }
        } else {
            // Exact source programming, not a guessed general address mask.
            // The documented lock does not alter reservation geometry.
            let expected = (MeSegmentMask::MASK_MIB
                .val(u64::from(self.request.mib).wrapping_neg() & 0x7ffff)
                + MeSegmentMask::STOLEN_ENABLE::SET)
                .value;
            if mask.get() & !MeSegmentMask::LOCK::SET.value != expected {
                return Err(Error::InvalidUma);
            }
        }
        Ok(())
    }
    fn data(self) -> u32 {
        (DramDone::BASE_MIB.val((self.base >> 20) as u32)
            + DramDone::STATUS.val(self.status as u32)
            + DramDone::TYPE.val(1))
        .value
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DramAction {
    NoAcknowledgement,
    NonPowerCycleReset,
    PowerCycleReset,
    Ignored3,
    Ignored4,
    Ignored5,
    GlobalReset,
    ContinuePost,
}
impl DramAction {
    pub fn parse(raw: u8) -> Result<Self, Error> {
        Ok(match raw {
            0 => Self::NoAcknowledgement,
            1 => Self::NonPowerCycleReset,
            2 => Self::PowerCycleReset,
            3 => Self::Ignored3,
            4 => Self::Ignored4,
            5 => Self::Ignored5,
            6 => Self::GlobalReset,
            7 => Self::ContinuePost,
            _ => return Err(Error::InvalidAction(raw)),
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BclkSkip {
    UltOrUlx,
    WatchdogFailure,
}
#[derive(Debug, Clone, Copy)]
pub struct BclkPolicy {
    pub ult_or_ulx: bool,
    pub oc_watchdog_control: u32,
}
impl BclkPolicy {
    fn validate(self) -> Result<(), Error> {
        if self.ult_or_ulx {
            return Err(Error::BclkSkipped(BclkSkip::UltOrUlx));
        }
        if self.oc_watchdog_control & (1 << 23) != 0 {
            return Err(Error::BclkSkipped(BclkSkip::WatchdogFailure));
        }
        Ok(())
    }
}
/// Raw firmware response, not a measured or assumed BCLK frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BclkResponse {
    pub data: u32,
    pub warm_reset_requested: bool,
}

fn response_header(command: Command, response: &[u8]) -> Result<&[u8], Error> {
    let (header, rest) =
        MkhiHeader::ref_from_prefix(response).map_err(|_| Error::ResponseHeader)?;
    if header.group != command.group()
        || header.opcode != (command.opcode() | 0x80)
        || header.reserved != 0
    {
        return Err(Error::ResponseHeader);
    }
    if header.result != 0 {
        return Err(Error::FirmwareResult(header.result));
    }
    Ok(rest)
}
pub fn parse_response(command: Command, response: &[u8]) -> Result<[u8; 4], Error> {
    let message = MkhiU32Message::ref_from_bytes(response).map_err(|_| Error::ResponseSize {
        expected: core::mem::size_of::<MkhiU32Message>(),
        actual: response.len(),
    })?;
    response_header(command, message.header.as_bytes())?;
    Ok(message.data.get().to_le_bytes())
}

pub struct Cse<H, M, W, R> {
    device: EcamDevice,
    bound_base: usize,
    transport: Transport<H, M, W, R>,
}
pub type MmioCse = Cse<
    fstart_core::mmio::MmioRegister<u32, Csr::Register>,
    fstart_core::mmio::MmioRegister<u32, Csr::Register>,
    fstart_core::mmio::MmioRegister<u32, Fifo::Register>,
    fstart_core::mmio::MmioRegister<u32, Fifo::Register>,
>;
impl MmioCse {
    /// Bind an already-enabled BAR; does not silently allocate or remap one.
    /// # Safety
    /// Exclusively own HECI1, its ECAM and mapped BAR as in Transport::at.
    pub unsafe fn bind(device: EcamDevice, budgets: Budgets) -> Result<Self, Error> {
        let base = decoded_bar(&device)?;
        if device.read16(4) & 6 != 6 {
            return Err(Error::InvalidBar);
        }
        let status = FirmwareStatus::read(&device)?;
        let transport = unsafe { MmioTransport::at(base, budgets)? };
        if !status.is_debug_mode() {
            transport.geometry()?;
        }
        Ok(Self {
            device,
            bound_base: base,
            transport,
        })
    }
}
impl<H, M, W, R> Cse<H, M, W, R>
where
    H: Readable<T = u32, R = Csr::Register> + Writeable<T = u32, R = Csr::Register>,
    M: Readable<T = u32, R = Csr::Register>,
    W: Writeable<T = u32, R = Fifo::Register>,
    R: Readable<T = u32, R = Fifo::Register>,
{
    fn validate_binding(&self) -> Result<FirmwareStatus, Error> {
        if decoded_bar(&self.device)? != self.bound_base {
            return Err(Error::InvalidBar);
        }
        if self.device.read16(4) & 6 != 6 {
            return Err(Error::InvalidBar);
        }
        FirmwareStatus::read(&self.device)
    }
    pub fn status(&self) -> Result<FirmwareStatus, Error> {
        self.validate_binding()
    }
    /// Non-writing DID eligibility/request preflight, before destructive training.
    /// Returns the unchanged HECI resource for the owner to check against RAM;
    /// neither reserves it nor constructs a ValidatedDram or initializes HECI.
    /// The same guards run again immediately before the actual DID message.
    pub(crate) fn preflight_did(&self, request: UmaRequest) -> Result<usize, Error> {
        let status = self.validate_binding()?;
        if status.is_debug_mode() {
            if request.mib != 0 {
                return Err(Error::InvalidUma);
            }
            return Ok(self.bound_base);
        }
        if status.initialized_without_uma() && request.mib != 0 {
            // Native romstage_native.c: the CSME stopped waiting for DID (a
            // warm reset dropped its UMA, or the bootblock was slow) but still
            // acknowledges a late one, and its reply may ask for a reset.
            fstart_log::warn!("CSE: running without UMA; sending DRAM Init Done anyway");
        }
        let current =
            UmaRequest::from_shadow(self.device.read32(0x44))?.ok_or(Error::InvalidUma)?;
        if current != request {
            return Err(Error::InvalidUma);
        }
        if !status.allows(Command::DramInitDone) {
            return Err(Error::Unsupported);
        }
        self.initialization_needed(status)?;
        Ok(self.bound_base)
    }
    // Shared, non-writing preconditions of the actual initializer. A ready
    // transport retains its existing fast path even when reinitialization is
    // forbidden; otherwise reject known firmware/geometry failure before MRC.
    fn initialization_needed(&self, status: FirmwareStatus) -> Result<bool, Error> {
        self.transport.geometry()?;
        if self.transport.ready().is_ok() {
            return Ok(false);
        }
        if !status.initialization_allowed() {
            return Err(Error::Firmware);
        }
        Ok(true)
    }
    pub fn initialize(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        let status = self.validate_binding()?;
        if !self.initialization_needed(status)? {
            return Ok(());
        }
        // Byte-width update: preserve neighbours and unrelated HIDM bits.
        self.device.write8(0xa0, self.device.read8(0xa0) & !3);
        self.transport.initialize(delay)?;
        Ok(())
    }
    pub fn reset_interface(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        let status = self.validate_binding()?;
        if !status.initialization_allowed() {
            return Err(Error::Firmware);
        }
        self.transport.reset(delay)?;
        Ok(())
    }
    fn exchange(
        &mut self,
        command: Command,
        data: u32,
        delay: &mut impl DelayNs,
    ) -> Result<[u8; 4], Error> {
        let status = self.validate_binding()?;
        if !status.allows(command) {
            return Err(Error::Unsupported);
        }
        // The only local arrays are the exact 8-byte protocol request/response.
        let request = command.request(data);
        let mut response = [0u8; core::mem::size_of::<MkhiU32Message>()];
        let count = self
            .transport
            .send_receive(request.as_bytes(), &mut response, 0, 7, delay)?;
        parse_response(command, &response[..count])
    }
    /// Re-read the actual SA reservation immediately before the message. Caller
    /// must provide its genuine, exclusively owned SKL/KBL root ECAM function.
    pub fn dram_init_done(
        &mut self,
        sa: &EcamDevice,
        dram: ValidatedDram,
        delay: &mut impl DelayNs,
    ) -> Result<DramAction, Error> {
        self.preflight_did(dram.request)?;
        super::early_sa::validate_host(sa).map_err(|_| Error::WrongDevice)?;
        dram.validate_me_segment(sa)?;
        let data = self.exchange(Command::DramInitDone, dram.data(), delay)?;
        if data[1..] != [0, 0, 0] {
            return Err(Error::ResponseHeader);
        }
        DramAction::parse(data[0])
    }
    /// Actual BUP_HSIO response, retaining version/family rather than turning
    /// a failed query into a fabricated matching CRC.
    pub fn hsio_crc(&mut self, delay: &mut impl DelayNs) -> Result<(u16, u8, u8), Error> {
        if !self.validate_binding()?.allows(Command::HsioCrc) {
            return Err(Error::Unsupported);
        }
        let request = Command::HsioCrc.header();
        let mut response = [0; core::mem::size_of::<MkhiU32Message>()];
        let count = self
            .transport
            .send_receive(request.as_bytes(), &mut response, 0, 7, delay)?;
        let data = parse_response(Command::HsioCrc, &response[..count])?;
        let hsio = HsioCrcData::ref_from_bytes(&data).unwrap();
        Ok((hsio.crc.get(), hsio.version, hsio.family))
    }

    /// Native ICC SET_HSIO_SETTINGS. Success acknowledges the write; the caller
    /// must perform the required cold reset and must not continue normal boot.
    /// # Safety
    /// Table must be the trusted reference blob selected for the actual PCH.
    /// Own the HECI/ICC configuration phase exclusively; no replay after failure.
    pub unsafe fn write_hsio_settings(
        &mut self,
        table: &[u8],
        request: &mut [u8],
        response: &mut [u8],
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let header_len = core::mem::size_of::<IccSetHsioHeader>();
        let reply_len = core::mem::size_of::<IccHeader>();
        if !(2..=2552).contains(&table.len())
            || request.len() < header_len + table.len()
            || response.len() < reply_len
        {
            return Err(Error::InvalidHsioBuffer);
        }
        if !self.validate_binding()?.allows(Command::HsioCrc) {
            return Err(Error::Unsupported);
        }
        let header = IccSetHsioHeader {
            icc: IccHeader {
                api: U32::new(0x0004_0000),
                command: U32::new(0x19),
                result: U32::new(0),
                length: U32::new((4 + table.len()) as u32),
                reserved: U32::new(0),
            },
            flags: U32::new(0),
        };
        request[..header_len].copy_from_slice(header.as_bytes());
        request[header_len..header_len + table.len()].copy_from_slice(table);
        let count = self.transport.send_receive(
            &request[..header_len + table.len()],
            response,
            0,
            8,
            delay,
        )?;
        let (reply, _) =
            IccHeader::ref_from_prefix(response.get(..count).ok_or(Error::ResponseHeader)?)
                .map_err(|_| Error::ResponseSize {
                    expected: reply_len,
                    actual: count,
                })?;
        if reply.api.get() != 0x0004_0000
            || reply.command.get() != 0x19
            || reply.reserved.get() != 0
            || reply.length.get() as usize != count - reply_len
        {
            return Err(Error::ResponseHeader);
        }
        if reply.result.get() != 0 {
            return Err(Error::IccResult(reply.result.get()));
        }
        Ok(())
    }

    pub fn bclk(
        &mut self,
        policy: BclkPolicy,
        delay: &mut impl DelayNs,
    ) -> Result<BclkResponse, Error> {
        policy.validate()?;
        let data = u32::from_le_bytes(self.exchange(Command::Bclk, 0, delay)?);
        Ok(BclkResponse {
            data,
            warm_reset_requested: data & IccData::WARM_RESET::SET.value != 0,
        })
    }
}
#[cfg(test)]
#[path = "cse_tests.rs"]
pub(crate) mod tests;
