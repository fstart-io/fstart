// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h *Gen2 runtime mailbox* (distinct from Gen1 flash directories),
//! the same C2P protocol on Raven/Picasso and Zeppelin at +0x10570 of the PSP
//! BAR. Cold/non-SMM only; no recovery, Sx/resume, fusing, flash writes, abort or reset commands.
use crate::fam17h::tsc::DelayNs;
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering, compiler_fence},
};
use tock_registers::{interfaces::Readable, register_bitfields, registers::InMemoryRegister};
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32, U64},
};

pub const COMMAND_OFFSET: usize = 0x10570;
pub const BUFFER_OFFSET: usize = COMMAND_OFFSET + 4;
const POLLS: usize = 10_000; // 100-us delays: nominal 1s, NOT a measured deadline.
/// Default/query commands' buffer, 32-byte aligned.
const QUERY_BYTES: usize = 32;
/// Room for the largest request, [`SmmInfoWire`], 32-byte aligned.
const BUFFER_BYTES: usize = size_of::<SmmInfoWire>().next_multiple_of(32);
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
register_bitfields![u32, Command [
    STATUS OFFSET(0) NUMBITS(16) [], CODE OFFSET(16) NUMBITS(8) [],
    RECOVERY OFFSET(30) NUMBITS(1) [], READY OFFSET(31) NUMBITS(1) []
]];

/// Every request starts with this; the PSP writes `status` (and `size`).
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    size: U32<LittleEndian>,
    status: U32<LittleEndian>,
}
impl Header {
    fn of<W>() -> Self {
        Self {
            size: (size_of::<W>() as u32).into(),
            status: 0.into(),
        }
    }
}
/// Default and query commands: the PSP answers in `value`.
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Wire {
    header: Header,
    value: U32<LittleEndian>,
    reserved: [u8; 20],
}
const _: () = assert!(size_of::<Wire>() == QUERY_BYTES);

/// An x86 memory dword the PSP uses from its side of SMM: `mask` selects the
/// bits, `value` what they are set to or are expected to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmmRegister {
    pub address: u64,
    pub mask: u32,
    pub value: u32,
}
/// The southbridge registers of [`SmmInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmiRegisters {
    /// Written to raise an SMI.
    pub trigger: SmmRegister,
    /// Expected values while SMIs can be taken.
    pub smi_enable: SmmRegister,
    pub eos: SmmRegister,
    pub psp_smi_enable: SmmRegister,
}
/// MBOX_BIOS_CMD_SMM_INFO: where SMM is and how the PSP reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmmInfo {
    /// SMM_ADDR and SMM_MASK (without TValid and the memory types).
    pub tseg_base: u64,
    pub tseg_mask: u64,
    /// PSP-to-core buffer in TSEG.
    pub p2c_buffer: u64,
    pub p2c_size: u64,
    pub registers: SmiRegisters,
    /// Core-to-PSP buffer for SMM commands and the flag that marks them,
    /// both in TSEG.
    pub c2p_buffer: u64,
    pub smm_flag: u64,
}
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RegisterWire {
    address: U64<LittleEndian>,
    /// 0 I/O, 1 memory.
    address_type: U32<LittleEndian>,
    /// 0 byte, 1 word, 2 dword.
    width: U32<LittleEndian>,
    mask: U32<LittleEndian>,
    value: U32<LittleEndian>,
}
impl RegisterWire {
    /// A memory dword; the trigger's masks are an AND and an OR mask.
    fn memory_dword(address: u64, mask: u32, value: u32) -> Self {
        Self {
            address: address.into(),
            address_type: 1.into(),
            width: 2.into(),
            mask: mask.into(),
            value: value.into(),
        }
    }
    fn expect(register: SmmRegister) -> Self {
        Self::memory_dword(register.address, register.mask, register.value)
    }
    fn trigger(register: SmmRegister) -> Self {
        Self::memory_dword(register.address, !register.mask, register.value)
    }
}
/// The SmmInfo buffer of PSP Gen2, 0x110 bytes as the vendor ZP firmware
/// sends it.
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SmmInfoWire {
    header: Header,
    tseg_base: U64<LittleEndian>,
    tseg_mask: U64<LittleEndian>,
    p2c_buffer: U64<LittleEndian>,
    p2c_size: U64<LittleEndian>,
    trigger: RegisterWire,
    smi_enable: RegisterWire,
    eos: RegisterWire,
    psp_smi_enable: RegisterWire,
    reserved: [RegisterWire; 5],
    c2p_buffer: U64<LittleEndian>,
    smm_flag: U64<LittleEndian>,
}
const _: () = assert!(size_of::<SmmInfoWire>() == 0x110);
impl From<&SmmInfo> for SmmInfoWire {
    fn from(info: &SmmInfo) -> Self {
        Self {
            header: Header::of::<Self>(),
            tseg_base: info.tseg_base.into(),
            tseg_mask: info.tseg_mask.into(),
            p2c_buffer: info.p2c_buffer.into(),
            p2c_size: info.p2c_size.into(),
            trigger: RegisterWire::trigger(info.registers.trigger),
            smi_enable: RegisterWire::expect(info.registers.smi_enable),
            eos: RegisterWire::expect(info.registers.eos),
            psp_smi_enable: RegisterWire::expect(info.registers.psp_smi_enable),
            reserved: FromZeros::new_zeroed(),
            c2p_buffer: info.c2p_buffer.into(),
            smm_flag: info.smm_flag.into(),
        }
    }
}

/// Permanently resident, coherent identity-DMA storage. A request is fully
/// written before publication; no writable view or reclamation is exposed.
#[repr(C, align(32))]
pub struct Buffer {
    bytes: UnsafeCell<MaybeUninit<[u8; BUFFER_BYTES]>>,
}
impl Buffer {
    pub const fn new() -> Self {
        Self {
            bytes: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
    pub fn allocation_range(&self) -> (u64, u64) {
        let start = self as *const Self as usize as u64;
        (start, start + size_of::<Self>() as u64)
    }
    fn prepare<W: IntoBytes + Immutable>(&mut self, request: &W) {
        const { assert!(size_of::<W>() <= BUFFER_BYTES) };
        // Prior successful delivery ended DMA; errors never reach preparation again.
        unsafe {
            self.bytes
                .get()
                .cast::<u8>()
                .copy_from_nonoverlapping(request.as_bytes().as_ptr(), size_of::<W>());
        }
        compiler_fence(Ordering::SeqCst);
    }
    fn snapshot<W: FromBytes>(&self) -> W {
        compiler_fence(Ordering::SeqCst);
        // Transport completion must precede this coherent DRAM snapshot; W is
        // unaligned and fits the buffer.
        unsafe { self.bytes.get().cast::<W>().read_volatile() }
    }
}
impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Exclusive correct-width device access. No concurrent SMM/PSP-mailbox owner.
/// # Safety
/// A successful READY+zero-CODE completion must mean the synchronous device has
/// finished all accesses to its buffer. Never fabricate completion/cancel DMA;
/// pointer publication precedes command submission, with ordered device writes.
pub unsafe trait Transport {
    fn command(&mut self) -> u32;
    /// # Safety
    /// Address points to initialized, aligned, reserved coherent DMA storage,
    /// retained to reset. No prior device operation can still use that storage.
    unsafe fn buffer_pointer(&mut self, address: u64);
    /// # Safety
    /// A supported command and valid published buffer; its semantic preconditions
    /// hold. Buffer remains retained on failure. No retry of an uncertain request.
    unsafe fn submit(&mut self, value: u32);
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Placement,
    AlreadyAttempted,
    Poisoned,
    BootDone,
    Unavailable,
    Recovery,
    ReadyTimeout,
    CompletionTimeout,
    Delivery(u16),
    BufferStatus(u32),
    BufferSize(u32),
}
impl Error {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Placement => "Placement",
            Self::AlreadyAttempted => "AlreadyAttempted",
            Self::Poisoned => "Poisoned",
            Self::BootDone => "BootDone",
            Self::Unavailable => "Unavailable",
            Self::Recovery => "Recovery",
            Self::ReadyTimeout => "ReadyTimeout",
            Self::CompletionTimeout => "CompletionTimeout",
            Self::Delivery(_) => "Delivery",
            Self::BufferStatus(_) => "BufferStatus",
            Self::BufferSize(_) => "BufferSize",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Poisoned,
    BootDone,
}
/// No transport/buffer extraction or retry after an error. Dropping this owner
/// does NOT cancel or reclaim anything; its static borrow remains reserved to reset.
pub struct Mailbox<T> {
    io: T,
    buffer: &'static mut Buffer,
    address: u64,
    state: State,
    not_send_sync: core::marker::PhantomData<*mut ()>,
}
fn placement(start: u64) -> Result<(), Error> {
    if start == 0
        || start & 31 != 0
        || start
            .checked_add(size_of::<Buffer>() as u64)
            .is_none_or(|end| end > 0x1_0000_0000)
    {
        return Err(Error::Placement);
    }
    Ok(())
}
fn claim(flag: &AtomicBool) -> Result<(), Error> {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| Error::AlreadyAttempted)
}
impl<T: Transport> Mailbox<T> {
    /// One owner/attempt per boot, even after ordinary errors or Drop.
    /// # Safety
    /// Cold BSP/non-SMM only. Exact buffer allocation must be reserved/excluded
    /// from allocators/payload maps, readable/writable coherent identity-mapped
    /// DRAM below4GiB until reset, including on failure. Exclude DMA by unrelated
    /// agents, aliases, reset, NMI/SMI/SMM and other mailbox users throughout use.
    /// Cache/mapping/transport geometry and delay behavior remain stable. CLI is
    /// not exclusion. No hardware writes occur on placement/claim rejection.
    pub unsafe fn new(io: T, buffer: &'static mut Buffer) -> Result<Self, Error> {
        let address = buffer.allocation_range().0;
        placement(address)?;
        claim(&ATTEMPTED)?;
        Ok(Self {
            io,
            buffer,
            address,
            state: State::Ready,
            not_send_sync: core::marker::PhantomData,
        })
    }
    fn wait(&mut self, completion: bool, delay: &mut impl DelayNs) -> Result<u32, Error> {
        for _ in 0..POLLS {
            let value = self.io.command();
            if value == u32::MAX {
                return Err(Error::Unavailable);
            }
            let register = InMemoryRegister::<u32, Command::Register>::new(value);
            if register.is_set(Command::RECOVERY) {
                return Err(Error::Recovery);
            }
            if register.is_set(Command::READY) && register.read(Command::CODE) == 0 {
                return Ok(value);
            }
            delay.delay_us(100);
        }
        Err(if completion {
            Error::CompletionTimeout
        } else {
            Error::ReadyTimeout
        })
    }
    fn transact<W: FromBytes + IntoBytes + Immutable>(
        &mut self,
        code: u8,
        request: &W,
        delay: &mut impl DelayNs,
    ) -> Result<W, Error> {
        match self.state {
            State::Poisoned => return Err(Error::Poisoned),
            State::BootDone => return Err(Error::BootDone),
            State::Ready => {}
        }
        // Sticky even if pre-submission wait fails or a transport/delay unwinds.
        // This is not global fail-stop; integration must stop after partial effects.
        self.state = State::Poisoned;
        self.wait(false, delay)?;
        self.buffer.prepare(request);
        // SAFETY: constructor/preparation/readiness retain the validated buffer;
        // only the private supported command wrappers can reach this trigger.
        unsafe {
            self.io.buffer_pointer(self.address);
            self.io.submit(Command::CODE.val(u32::from(code)).value);
        }
        let response = self.wait(true, delay)?;
        let delivery =
            InMemoryRegister::<u32, Command::Register>::new(response).read(Command::STATUS) as u16;
        if delivery != 0 {
            return Err(Error::Delivery(delivery));
        }
        let header: Header = self.buffer.snapshot();
        if header.size.get() != size_of::<W>() as u32 {
            return Err(Error::BufferSize(header.size.get()));
        }
        if header.status.get() != 0 {
            return Err(Error::BufferStatus(header.status.get()));
        }
        self.state = State::Ready;
        Ok(self.buffer.snapshot())
    }
    fn query(&mut self, code: u8, delay: &mut impl DelayNs) -> Result<u32, Error> {
        let request = Wire {
            header: Header::of::<Wire>(),
            value: 0.into(),
            reserved: [0; 20],
        };
        self.transact(code, &request, delay)
            .map(|reply| reply.value.get())
    }
    /// Opaque PSP capability bits, not authentication or a protection claim.
    pub fn capabilities(&mut self, delay: &mut impl DelayNs) -> Result<u32, Error> {
        self.query(0x27, delay)
    }
    pub fn ftpm_capabilities(&mut self, delay: &mut impl DelayNs) -> Result<u32, Error> {
        self.query(0x05, delay)
    }
    pub fn hsti_state(&mut self, delay: &mut impl DelayNs) -> Result<u32, Error> {
        self.query(0x14, delay)
    }
    /// SmmInfo, sent from normal mode: the PSP
    /// takes TSEG from it, also for the DF protection LockDFReg sets up.
    pub fn smm_info(&mut self, info: &SmmInfo, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.transact(0x02, &SmmInfoWire::from(info), delay)
            .map(|_| ())
    }
    /// LockDFReg of the vendor firmware: afterwards the Data Fabric registers
    /// can't be changed anymore. The last DF write must precede this, and
    /// [`Self::smm_info`] too: without it the lock breaks device DMA.
    pub fn lock_df_registers(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.query(0x1b, delay).map(|_| ())
    }
    /// Irreversible transition: PSP afterwards only honors SMM-space buffers.
    /// # Safety
    /// All native initialization/architectural CPU completion and required PSP
    /// services/protected SMM buffers/handlers are complete, validated and locked;
    /// memory remains reserved to reset. No later ordinary mailbox query is allowed.
    /// Not a substitute for SMM/ROM-Armor setup or safe OS handoff. On error the
    /// transition may already have happened: fail-stop, never retry/cancel/reset.
    pub unsafe fn boot_done(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.query(0x06, delay)?;
        self.state = State::BootDone;
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
#[path = "psp/native.rs"]
mod native;
#[cfg(target_arch = "x86_64")]
pub use native::Native;
#[cfg(test)]
#[path = "psp/tests.rs"]
mod tests;
