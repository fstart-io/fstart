// SPDX-License-Identifier: MIT OR Apache-2.0
//! Correct-width MMIO transport, using an already verified locked PSP aperture.
use super::{BUFFER_OFFSET, COMMAND_OFFSET, Command, Transport};
use tock_registers::{
    interfaces::{Readable, Writeable},
    registers::ReadWrite,
};

// The source writes one qword at +0x10574: only FOUR-byte aligned. A Rust
// ReadWrite<u64> reference would be unaligned; do not replace it with that or
// invent two dword writes. This tock Writeable performs the fixed-RAX qword MOV.
struct Pointer(usize);
impl Writeable for Pointer {
    type T = u64;
    type R = ();
    fn set(&self, value: u64) {
        unsafe {
            core::arch::asm!("mov qword ptr [{address}], rax", address = in(reg) self.0, in("rax") value, options(nostack, preserves_flags));
        }
    }
}
pub struct Native {
    command: *const ReadWrite<u32, Command::Register>,
    pointer: Pointer,
    not_send_sync: core::marker::PhantomData<*mut ()>,
}
impl Native {
    /// The mailbox of the PSP whose BAR is at `base`. No SMN discovery,
    /// implicit locking, resetting or mailbox probing.
    /// # Safety
    /// Caller verified that PSP BAR high=0 and low=`base`, enabled+locked, the
    /// aperture is correctly decoded/identity UC mapped, exact width
    /// transactions supported. Exclusive cold BSP access through reset; exclude
    /// SMM/interrupt/reset/other bus owners and any competing mailbox transport.
    /// PSP's synchronous READY/zero-CODE acknowledgement must finish its DMA.
    pub unsafe fn at(base: usize) -> Self {
        Self {
            command: (base + COMMAND_OFFSET) as *const ReadWrite<u32, Command::Register>,
            pointer: Pointer(base + BUFFER_OFFSET),
            not_send_sync: core::marker::PhantomData,
        }
    }
}
// SAFETY: construction requires exclusive mapped exact-width transport and
// physical acknowledgement/DMA contracts; no successful result is fabricated.
unsafe impl Transport for Native {
    fn command(&mut self) -> u32 {
        unsafe { (*self.command).get() }
    }
    unsafe fn buffer_pointer(&mut self, address: u64) {
        self.pointer.set(address);
    }
    unsafe fn submit(&mut self, value: u32) {
        unsafe {
            (*self.command).set(value);
        }
    }
}
