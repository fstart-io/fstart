// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h *Gen2 runtime mailbox* (distinct from Gen1 flash directories),
//! the same C2P protocol on Raven/Picasso and Zeppelin at +0x10570 of the PSP
//! BAR. Cold/non-SMM only;
//! no recovery, Sx/resume, fusing, flash writes, abort or reset commands.
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering, compiler_fence},
};
use tock_registers::{interfaces::Readable, register_bitfields, registers::InMemoryRegister};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32},
};

pub const COMMAND_OFFSET: usize = 0x10570;
pub const BUFFER_OFFSET: usize = COMMAND_OFFSET + 4;
const POLLS: usize = 10_000; // 100-us delays: nominal 1s, NOT a measured deadline.
const BUFFER_BYTES: usize = 32;
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
register_bitfields![u32, Command [
    STATUS OFFSET(0) NUMBITS(16) [], CODE OFFSET(16) NUMBITS(8) [],
    RECOVERY OFFSET(30) NUMBITS(1) [], READY OFFSET(31) NUMBITS(1) []
]];

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Wire {
    size: U32<LittleEndian>,
    status: U32<LittleEndian>,
    value: U32<LittleEndian>,
    reserved: [u8; 20],
}
const _: () = assert!(size_of::<Wire>() == BUFFER_BYTES);
/// Permanently resident, coherent identity-DMA storage. All 32 bytes are
/// initialized before publication; no writable view or reclamation is exposed.
#[repr(C, align(32))]
pub struct Buffer {
    wire: UnsafeCell<MaybeUninit<Wire>>,
}
impl Buffer {
    pub const fn new() -> Self {
        Self {
            wire: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
    pub fn allocation_range(&self) -> (u64, u64) {
        let start = self as *const Self as usize as u64;
        (start, start + size_of::<Self>() as u64)
    }
    fn prepare(&mut self) {
        // Prior successful delivery ended DMA; errors never reach preparation again.
        unsafe {
            (*self.wire.get()).write(Wire {
                size: (BUFFER_BYTES as u32).into(),
                status: 0.into(),
                value: 0.into(),
                reserved: [0; 20],
            });
        }
        compiler_fence(Ordering::SeqCst);
    }
    fn snapshot(&self) -> Wire {
        compiler_fence(Ordering::SeqCst);
        // Transport completion must precede this coherent DRAM snapshot.
        unsafe { self.wire.get().cast::<Wire>().read_volatile() }
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
    fn wait(&mut self, completion: bool, delay_us: &mut impl FnMut(u32)) -> Result<u32, Error> {
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
            delay_us(100);
        }
        Err(if completion {
            Error::CompletionTimeout
        } else {
            Error::ReadyTimeout
        })
    }
    fn transact(&mut self, code: u8, delay_us: &mut impl FnMut(u32)) -> Result<u32, Error> {
        match self.state {
            State::Poisoned => return Err(Error::Poisoned),
            State::BootDone => return Err(Error::BootDone),
            State::Ready => {}
        }
        // Sticky even if pre-submission wait fails or a transport/delay unwinds.
        // This is not global fail-stop; integration must stop after partial effects.
        self.state = State::Poisoned;
        self.wait(false, delay_us)?;
        self.buffer.prepare();
        // SAFETY: constructor/preparation/readiness retain the validated buffer;
        // only the private supported command wrappers can reach this trigger.
        unsafe {
            self.io.buffer_pointer(self.address);
            self.io.submit(Command::CODE.val(u32::from(code)).value);
        }
        let response = self.wait(true, delay_us)?;
        let delivery =
            InMemoryRegister::<u32, Command::Register>::new(response).read(Command::STATUS) as u16;
        if delivery != 0 {
            return Err(Error::Delivery(delivery));
        }
        let reply = self.buffer.snapshot();
        if reply.size.get() != BUFFER_BYTES as u32 {
            return Err(Error::BufferSize(reply.size.get()));
        }
        if reply.status.get() != 0 {
            return Err(Error::BufferStatus(reply.status.get()));
        }
        self.state = State::Ready;
        Ok(reply.value.get())
    }
    /// Opaque PSP capability bits, not authentication or a protection claim.
    pub fn capabilities(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<u32, Error> {
        self.transact(0x27, delay_us)
    }
    pub fn ftpm_capabilities(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<u32, Error> {
        self.transact(0x05, delay_us)
    }
    pub fn hsti_state(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<u32, Error> {
        self.transact(0x14, delay_us)
    }
    /// LockDFReg of the vendor firmware: afterwards the Data Fabric registers
    /// can't be changed anymore. The last DF write must precede this.
    pub fn lock_df_registers(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<(), Error> {
        self.transact(0x1b, delay_us).map(|_| ())
    }
    /// Irreversible transition: PSP afterwards only honors SMM-space buffers.
    /// # Safety
    /// All native initialization/architectural CPU completion and required PSP
    /// services/protected SMM buffers/handlers are complete, validated and locked;
    /// memory remains reserved to reset. No later ordinary mailbox query is allowed.
    /// Not a substitute for SMM/ROM-Armor setup or safe OS handoff. On error the
    /// transition may already have happened: fail-stop, never retry/cancel/reset.
    pub unsafe fn boot_done(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<(), Error> {
        self.transact(0x06, delay_us)?;
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
