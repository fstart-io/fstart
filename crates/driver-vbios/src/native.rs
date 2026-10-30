// SPDX-License-Identifier: MIT
//! rsemu adapter. All board policy remains in the shared VBIOS front end.
//!
//! The bus has no borrowed pointer or host mapping. It accesses ACTIVE only
//! during synchronous, BUSY-serialized execution, with no
//! Context reference alive across a CPU call. Neither bus nor CPU escapes the
//! enclosing Machine. CPU exits are serviced after its internal lock is released.
use super::{Error, Registers, context, step_at};
use alloc::sync::Arc;
use rsemu::{
    core::{
        error::BusError,
        space::{AccessConstraints, AddressSpace, MemAttrs, MemOps, MemResult, Region},
    },
    cpu::x86::{
        Config, Regs, X86,
        embedding::{Exit, StepOptions},
    },
};

#[derive(Debug)]
struct Bus {
    ports: bool,
}
impl Bus {
    fn access(&self, offset: u64, size: usize, debug: bool) -> Result<(u32, u32), BusError> {
        let ctx = context();
        if ctx.error.is_some() {
            return Err(BusError::BadAccess);
        }
        let limit = if self.ports { 0x10000 } else { 0x1_0000_0000 };
        let end = offset.saturating_add(size as u64);
        let physical = !self.ports && ctx.physical(offset as u32, size as u32);
        let overlaps = !self.ports
            && ((offset < 0xc0000 && end > 0xa0000)
                || ctx
                    .windows
                    .iter()
                    .any(|w| offset < w.base + w.size && end > w.base));
        // Wide RAM operands (x87/SSE) are fine. Never split a wide or mixed
        // device transaction into invented byte/dword hardware accesses.
        if !matches!(size, 1 | 2 | 4 | 8 | 16)
            || end > limit
            || (self.ports && size > 4)
            || (overlaps && (!physical || size > 4))
            || (debug && (self.ports || physical))
        {
            ctx.fail(Error::Bounds);
            return Err(BusError::BadAccess);
        }
        Ok((offset as u32, size as u32))
    }
}
impl MemOps for Bus {
    fn read(&self, offset: u64, dst: &mut [u8], attrs: MemAttrs) -> MemResult {
        let (address, width) = self.access(offset, dst.len(), attrs.debug)?;
        if dst.len() > 4 {
            let ctx = context();
            let Some(bytes) = ctx
                .memory
                .get(address as usize..address as usize + dst.len())
            else {
                ctx.fail(Error::Bounds);
                return Err(BusError::BadAccess);
            };
            dst.copy_from_slice(bytes);
            return Ok(());
        }
        let value = if self.ports {
            super::port_read(address as u16, width)
        } else {
            super::memory_read(address, width)
        };
        if context().error.is_some() {
            return Err(BusError::BadAccess);
        }
        dst.copy_from_slice(&value.to_le_bytes()[..dst.len()]);
        Ok(())
    }
    fn write(&self, offset: u64, src: &[u8], attrs: MemAttrs) -> MemResult {
        let (address, width) = self.access(offset, src.len(), attrs.debug)?;
        if src.len() > 4 {
            let ctx = context();
            let Some(bytes) = ctx
                .memory
                .get_mut(address as usize..address as usize + src.len())
            else {
                ctx.fail(Error::Bounds);
                return Err(BusError::BadAccess);
            };
            bytes.copy_from_slice(src);
            return Ok(());
        }
        let mut bytes = [0; 4];
        bytes[..src.len()].copy_from_slice(src);
        let value = u32::from_le_bytes(bytes);
        if self.ports {
            super::port_write(address as u16, width, value);
        } else {
            super::memory_write(address, width, value);
        }
        if context().error.is_some() {
            Err(BusError::BadAccess)
        } else {
            Ok(())
        }
    }
    fn constraints(&self) -> AccessConstraints {
        AccessConstraints::ANY
    }
}

pub(super) struct Emulator {
    cpu: X86,
}
impl Emulator {
    pub(super) fn new() -> Result<Self, Error> {
        let memory = Arc::new(AddressSpace::new("vbios-memory", 32));
        memory
            .topology()
            .map(
                Region::io("memory-bus", 0x1_0000_0000, Arc::new(Bus { ports: false })),
                0,
            )
            .map_err(|_| Error::Emulation)?;
        let ports = Arc::new(AddressSpace::new("vbios-ports", 16));
        ports
            .topology()
            .map(
                Region::io("port-bus", 0x10000, Arc::new(Bus { ports: true })),
                0,
            )
            .map_err(|_| Error::Emulation)?;
        // This selects the implemented ISA, not the host's CPUID. Exposing host
        // AVX/XSAVE to an SSE2 interpreter would silently change ROM paths.
        let cpu = X86::new(Config::X86_64);
        cpu.attach_space(memory);
        cpu.attach_io_space(ports);
        cpu.set_a20(true);
        cpu.step(); // architectural reset only; performs no bus access
        Ok(Self { cpu })
    }
    fn stack(&self) {
        let mut regs = self.cpu.regs();
        regs.ss = 0x7000;
        regs.rsp = 0xfffe;
        regs.cs = 0x1000;
        regs.rip = 0;
        self.cpu.set_regs(regs);
    }
    pub(super) fn post(&self, bdf: u32) -> bool {
        let mut regs = Regs::new();
        regs.cs = 0xc000;
        regs.rip = 3;
        regs.ss = 0x7000;
        regs.rsp = 0xfffa;
        regs.rax = u64::from(bdf);
        regs.rdx = 0x80;
        regs.eflags = 2;
        self.cpu.set_regs(regs);
        super::memory_write(0x7fffa, 2, 0);
        super::memory_write(0x7fffc, 2, 0x1000);
        self.run()
    }
    pub(super) fn int10(&self, regs: &mut Registers) -> bool {
        self.stack();
        self.set_regs(regs);
        if self.cpu.enter_software_interrupt(0x10).exit != Exit::Continue {
            context().fail(Error::Emulation);
            return false;
        }
        let completed = self.run();
        *regs = self.regs();
        completed
    }
    fn regs(&self) -> Registers {
        let r = self.cpu.regs();
        Registers {
            eax: r.rax as u32,
            ebx: r.rbx as u32,
            ecx: r.rcx as u32,
            edx: r.rdx as u32,
            esi: r.rsi as u32,
            edi: r.rdi as u32,
            es: r.es,
            flags: r.eflags as u16,
        }
    }
    fn set_regs(&self, r: &Registers) {
        let mut regs = self.cpu.regs();
        regs.rax = u64::from(r.eax);
        regs.rbx = u64::from(r.ebx);
        regs.rcx = u64::from(r.ecx);
        regs.rdx = u64::from(r.edx);
        regs.rsi = u64::from(r.esi);
        regs.rdi = u64::from(r.edi);
        regs.es = r.es;
        regs.eflags = u32::from(r.flags);
        self.cpu.set_regs(regs);
    }
    fn interrupt(&self, number: u8) -> bool {
        let vector = super::memory_read(u32::from(number) * 4, 4);
        let target = ((vector >> 16) << 4) + (vector & 0xffff);
        if (0xc0000..0xe0000).contains(&target) {
            self.cpu.enter_software_interrupt(number).exit == Exit::Continue
        } else {
            let mut regs = self.regs();
            if super::bios_interrupt(u32::from(number), &mut regs) == 0 {
                return false;
            }
            self.set_regs(&regs);
            true
        }
    }
    fn run(&self) -> bool {
        loop {
            if context().error.is_some() {
                return false;
            }
            let before = self.cpu.regs();
            let pc = u32::from(before.cs) << 16 | before.rip as u16 as u32;
            if step_at(pc) == 0 {
                return false;
            }
            // REP is capped within the interpreter, so neither a huge count nor
            // IF=0 can keep us away from the deadline indefinitely.
            let result = self.cpu.step_embedded(StepOptions::default());
            if context().error.is_some() {
                return false;
            }
            match result.exit {
                Exit::Continue | Exit::RepeatYield => (),
                Exit::SoftwareInterrupt(number) if self.interrupt(number) => (),
                Exit::Halted
                    if before.cs == 0x1000
                        && before.rip == 0
                        && self.cpu.regs().cs == 0x1000
                        && self.cpu.regs().rip == 1 =>
                {
                    return true;
                }
                _ => {
                    fstart_log::error!(
                        "VBIOS Rust exit at {}:{} (vector {})",
                        fstart_log::Hex(before.cs.into()),
                        fstart_log::Hex(before.rip),
                        match result.exit {
                            Exit::Exception { vector, .. } => u32::from(vector),
                            _ => 0xffff,
                        }
                    );
                    context().fail(Error::Emulation);
                    return false;
                }
            }
        }
    }
}
