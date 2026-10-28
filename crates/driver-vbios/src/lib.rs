// SPDX-License-Identifier: MIT
//! x86 VGA option-ROM initialization and VBE linear-framebuffer selection.
//! Native Rust x86 emulation with bounded private RAM, explicit MMIO windows,
//! and a deadline. No real-mode switch or low-RAM writes.
#![no_std]
#![cfg(target_arch = "x86_64")]
extern crate alloc;
mod native;
use alloc::{vec, vec::Vec};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};
use fstart_core::{
    mmio,
    pio::PioRegister,
    services::{FramebufferInfo, Timer},
};
use fstart_pci::{PciAddress, PciEcam, PciWindow};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    little_endian::{U16, U32},
};

const ROM_BASE: usize = 0xc0000;
const BUFFER: usize = 0x20000;
const GUEST_SIZE: usize = 0x110000;
static BUSY: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Busy,
    Rom,
    Bounds,
    Timeout,
    Interrupt,
    Emulation,
    Vbe,
    Mode,
}
impl Error {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Busy => "VBIOS busy",
            Self::Rom => "VBIOS ROM",
            Self::Bounds => "VBIOS memory bounds",
            Self::Timeout => "VBIOS timeout",
            Self::Interrupt => "VBIOS interrupt",
            Self::Emulation => "VBIOS execution",
            Self::Vbe => "VBE interface",
            Self::Mode => "VBE mode",
        }
    }
}
#[derive(Default)]
struct Registers {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
    esi: u32,
    edi: u32,
    es: u16,
    flags: u16,
}
struct Context<'a> {
    memory: Vec<u8>,
    pci: Option<&'a PciEcam>,
    windows: &'a [PciWindow],
    timer: &'a dyn Timer,
    started: u64,
    steps: u64,
    error: Option<Error>,
    last_memory: (u32, u32),
    last_port: (u16, u32),
}
impl Context<'_> {
    fn fail(&mut self, error: Error) {
        self.error.get_or_insert(error);
    }
    fn reset_deadline(&mut self) {
        self.started = self.timer.timestamp_us();
        self.steps = 0;
    }
    fn physical(&self, address: u32, width: u32) -> bool {
        let start = u64::from(address);
        let end = start + u64::from(width);
        (start >= 0xa0000 && end <= 0xc0000)
            || self
                .windows
                .iter()
                .any(|w| start >= w.base && end <= w.base + w.size)
    }
}
// No context or guest-RAM reference may survive an emulator call: its bus
// temporarily borrows the context through UnsafeCell instead.
struct Machine<'a> {
    context: UnsafeCell<Context<'a>>,
    emulator: native::Emulator,
}
impl<'a> Machine<'a> {
    fn new(context: Context<'a>) -> Result<Self, Error> {
        Ok(Self {
            context: UnsafeCell::new(context),
            emulator: native::Emulator::new()?,
        })
    }
    fn post(&self, bdf: u32) -> bool {
        self.emulator.post(bdf)
    }
    fn with<R>(&self, f: impl FnOnce(&mut Context<'_>) -> R) -> R {
        // SAFETY: BSP-only synchronous access; this helper never runs the CPU.
        unsafe { f(&mut *self.context.get()) }
    }
    fn bytes<const N: usize>(&self, start: usize) -> Option<[u8; N]> {
        self.with(|ctx| {
            ctx.memory
                .get(start..start.checked_add(N)?)?
                .try_into()
                .ok()
        })
    }
    fn int10(&self, regs: &mut Registers) -> Result<(), Error> {
        if let Some(error) = self.with(|ctx| ctx.error) {
            return Err(error);
        }
        self.with(|ctx| ctx.reset_deadline());
        let completed = self.emulator.int10(regs);
        if let Some(error) = self.with(|ctx| ctx.error) {
            return Err(error);
        }
        if !completed {
            return Err(Error::Emulation);
        }
        if regs.eax as u16 != 0x004f {
            return Err(Error::Vbe);
        }
        Ok(())
    }
    fn vbe(&self, function: u32, bx: u32, cx: u32) -> Result<Registers, Error> {
        self.with(|ctx| {
            ctx.memory[BUFFER..BUFFER + 512].fill(0);
            if function == 0x4f00 {
                ctx.memory[BUFFER..BUFFER + 4].copy_from_slice(b"VBE2");
            }
        });
        let mut regs = Registers {
            eax: function,
            ebx: bx,
            ecx: cx,
            es: (BUFFER >> 4) as u16,
            flags: 2,
            ..Default::default()
        };
        self.int10(&mut regs)?;
        Ok(regs)
    }
}
impl Drop for Machine<'_> {
    fn drop(&mut self) {
        ACTIVE.store(core::ptr::null_mut(), Ordering::Release);
    }
}
struct Guard {
    cf8: Option<u32>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        ACTIVE.store(core::ptr::null_mut(), Ordering::Release);
        if let Some(cf8) = self.cf8 {
            PioRegister::<u32>::new(0xcf8).set(cf8);
        }
        BUSY.store(false, Ordering::Release);
    }
}
fn context() -> &'static mut Context<'static> {
    let pointer = ACTIVE.load(Ordering::Acquire);
    // SAFETY: the private bus accesses this only during serialized execution.
    // The pointer's erased lifetimes are restored by that enclosing call's scope.
    unsafe { &mut *pointer.cast::<Context<'static>>() }
}
fn step_at(pc: u32) -> i32 {
    let ctx = context();
    ctx.steps += 1;
    if ctx.steps > 100_000_000
        || (ctx.steps & 4095 == 0
            && ctx.timer.timestamp_us().wrapping_sub(ctx.started) > 20_000_000)
    {
        fstart_log::error!(
            "VBIOS timeout: steps {} CS:IP {} MMIO {}={} port {}={}",
            ctx.steps,
            fstart_log::Hex(pc.into()),
            fstart_log::Hex(ctx.last_memory.0.into()),
            fstart_log::Hex(ctx.last_memory.1.into()),
            fstart_log::Hex(ctx.last_port.0.into()),
            fstart_log::Hex(ctx.last_port.1.into())
        );
        ctx.fail(Error::Timeout);
        return 0;
    }
    1
}
fn memory_read(address: u32, width: u32) -> u32 {
    let ctx = context();
    if ctx.error.is_some() {
        return u32::MAX;
    }
    if !matches!(width, 1 | 2 | 4) {
        ctx.fail(Error::Bounds);
        return u32::MAX;
    }
    if ctx.physical(address, width) {
        // SAFETY: exclusive, decoded GPU windows with resource-appropriate caching.
        let value = unsafe {
            match width {
                1 => u32::from(mmio::read8(address as *const u8)),
                2 => u32::from(mmio::read16_unaligned(address as *const u8)),
                4 => mmio::read32_unaligned(address as *const u8),
                _ => 0,
            }
        };
        ctx.last_memory = (address, value);
        return value;
    }
    if let Some(bytes) = ctx
        .memory
        .get(address as usize..address as usize + width as usize)
    {
        let mut value = [0; 4];
        value[..bytes.len()].copy_from_slice(bytes);
        return u32::from_le_bytes(value);
    }
    fstart_log::error!(
        "VBIOS read outside memory at {} width {}",
        fstart_log::Hex(address.into()),
        width
    );
    ctx.fail(Error::Bounds);
    u32::MAX
}
fn memory_write(address: u32, width: u32, value: u32) {
    let ctx = context();
    if ctx.error.is_some() {
        return;
    }
    if !matches!(width, 1 | 2 | 4) {
        ctx.fail(Error::Bounds);
        return;
    }
    if ctx.physical(address, width) {
        ctx.last_memory = (address, value);
        // SAFETY: as in the read callback.
        unsafe {
            match width {
                1 => mmio::write8(address as *mut u8, value as u8),
                2 => mmio::write16_unaligned(address as *mut u8, value as u16),
                4 => mmio::write32_unaligned(address as *mut u8, value),
                _ => (),
            }
        };
        return;
    }
    if let Some(bytes) = ctx
        .memory
        .get_mut(address as usize..address as usize + width as usize)
    {
        bytes.copy_from_slice(&value.to_le_bytes()[..width as usize]);
        return;
    }
    fstart_log::error!(
        "VBIOS write outside memory at {} width {}",
        fstart_log::Hex(address.into()),
        width
    );
    ctx.fail(Error::Bounds);
}
fn port_read(port: u16, width: u32) -> u32 {
    let ctx = context();
    if ctx.error.is_some() {
        return u32::MAX;
    }
    if !matches!(width, 1 | 2 | 4) || u32::from(port) + width > 0x10000 {
        ctx.fail(Error::Bounds);
        return u32::MAX;
    }
    if ctx.pci.is_none() {
        ctx.fail(Error::Bounds);
        return u32::MAX;
    }
    let value = match width {
        1 => PioRegister::<u8>::new(port).get().into(),
        2 => PioRegister::<u16>::new(port).get().into(),
        4 => PioRegister::<u32>::new(port).get(),
        _ => 0,
    };
    ctx.last_port = (port, value);
    value
}
fn port_write(port: u16, width: u32, value: u32) {
    let ctx = context();
    if ctx.error.is_some() {
        return;
    }
    if !matches!(width, 1 | 2 | 4) || u32::from(port) + width > 0x10000 {
        ctx.fail(Error::Bounds);
        return;
    }
    if ctx.pci.is_none() {
        ctx.fail(Error::Bounds);
        return;
    }
    ctx.last_port = (port, value);
    match width {
        1 => PioRegister::<u8>::new(port).set(value as u8),
        2 => PioRegister::<u16>::new(port).set(value as u16),
        4 => PioRegister::<u32>::new(port).set(value),
        _ => (),
    }
}
fn bios_interrupt(number: u32, regs: &mut Registers) -> i32 {
    let ctx = context();
    if ctx.error.is_some() {
        return 0;
    }
    let ax = regs.eax as u16;
    match number {
        0x10 | 0x42 | 0x6d => {
            // Legacy text services before the ROM installs its own INT10.
            if ax >> 8 == 0x0f {
                regs.eax = 0x5003;
            }
        }
        0x11 => regs.eax = 0,
        0x12 => regs.eax = 640,
        0x15 => {
            if ax == 0x8600 {
                ctx.timer
                    .delay_us(u64::from((regs.ecx & 0xffff) << 16 | (regs.edx & 0xffff)));
                regs.flags &= !1;
            } else {
                regs.eax = 0x8600;
                regs.flags |= 1;
            }
        }
        0x16 => regs.flags |= 1 << 6, // no pending keyboard input
        0x1a => {
            if ax >> 8 == 0 {
                let ticks = ctx.timer.timestamp_us() / 54925;
                regs.ecx = (ticks >> 16) as u32;
                regs.edx = ticks as u16 as u32;
                regs.eax = 0;
                return 1;
            }
            let Some(pci) = ctx.pci else {
                ctx.fail(Error::Interrupt);
                return 0;
            };
            regs.flags &= !1;
            match ax {
                0xb101 => {
                    regs.eax = 1;
                    regs.ebx = 0x0210;
                    regs.ecx = u32::from(pci.devices().map(|a| a.bus()).max().unwrap_or(0));
                    regs.edx = 0x20494350;
                }
                0xb102 | 0xb103 => {
                    let found = pci
                        .devices()
                        .filter(|&a| {
                            pci.device(a).is_some_and(|d| {
                                if ax == 0xb102 {
                                    d.read32(0) == ((regs.ecx & 0xffff) << 16 | (regs.edx & 0xffff))
                                } else {
                                    d.read32(8) >> 8 == regs.ecx & 0xffffff
                                }
                            })
                        })
                        .nth((regs.esi & 0xffff) as usize);
                    if let Some(a) = found {
                        regs.ebx = (u32::from(a.bus()) << 8)
                            | (u32::from(a.device()) << 3)
                            | u32::from(a.function());
                        regs.eax = 0;
                    } else {
                        regs.eax = 0x8600;
                        regs.flags |= 1;
                    }
                }
                0xb108..=0xb10d => {
                    let address = PciAddress::new(
                        0,
                        (regs.ebx >> 8) as u8,
                        ((regs.ebx >> 3) & 31) as u8,
                        (regs.ebx & 7) as u8,
                    );
                    let offset = regs.edi as u16;
                    let result = pci.device(address).and_then(|d| match ax {
                        0xb108 => d.try_read8(offset).map(u32::from),
                        0xb109 => d.try_read16(offset).map(u32::from),
                        0xb10a => d.try_read32(offset),
                        0xb10b => d.try_write8(offset, regs.ecx as u8).map(|_| regs.ecx),
                        0xb10c => d.try_write16(offset, regs.ecx as u16).map(|_| regs.ecx),
                        0xb10d => d.try_write32(offset, regs.ecx).map(|_| regs.ecx),
                        _ => None,
                    });
                    if let Some(value) = result {
                        regs.ecx = value;
                        regs.eax &= !0xff00;
                    } else {
                        regs.eax = 0x8700;
                        regs.flags |= 1;
                    }
                }
                _ => {
                    regs.eax = 0x8100;
                    regs.flags |= 1;
                }
            }
        }
        _ => {
            fstart_log::error!(
                "VBIOS unsupported interrupt {} AX {}",
                fstart_log::Hex(number.into()),
                fstart_log::Hex(ax.into())
            );
            ctx.fail(Error::Interrupt);
            return 0;
        }
    }
    1
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct RomHeader {
    signature: U16,
    blocks: u8,
    reserved: [u8; 21],
    pcir: U16,
}
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct Pcir {
    signature: [u8; 4],
    vendor: U16,
    device: U16,
    reserved: [u8; 8],
    blocks: U16,
    revision: U16,
    code_type: u8,
    indicator: u8,
    tail: U16,
}
fn rom_image(rom: &[u8], id: u32) -> Result<&[u8], Error> {
    let (header, _) = RomHeader::ref_from_prefix(rom).map_err(|_| Error::Rom)?;
    if header.signature.get() != 0xaa55 || header.blocks == 0 {
        return Err(Error::Rom);
    }
    let size = usize::from(header.blocks) * 512;
    let bytes = rom
        .get(..size)
        .filter(|b| b.len() <= 0x20000)
        .ok_or(Error::Rom)?;
    let pcir_bytes = bytes.get(header.pcir.get() as usize..).ok_or(Error::Rom)?;
    let (pcir, _) = Pcir::ref_from_prefix(pcir_bytes).map_err(|_| Error::Rom)?;
    if pcir.signature != *b"PCIR"
        || pcir.vendor.get() != id as u16
        || pcir.device.get() != (id >> 16) as u16
        || pcir.code_type != 0
        || bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) != 0
    {
        return Err(Error::Rom);
    }
    Ok(bytes)
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, KnownLayout, Immutable, Unaligned)]
struct Mode {
    attributes: U16,
    bank: [u8; 14],
    stride: U16,
    width: U16,
    height: U16,
    chars: [u8; 2],
    planes: u8,
    bpp: u8,
    banks: u8,
    model: u8,
    bank_size: u8,
    pages: u8,
    reserved_page: u8,
    red_size: u8,
    red_pos: u8,
    green_size: u8,
    green_pos: u8,
    blue_size: u8,
    blue_pos: u8,
    reserved_size: u8,
    reserved_pos: u8,
    direct: u8,
    base: U32,
    offscreen: U32,
    offscreen_size: U16,
    linear_stride: U16,
    linear_pages: [u8; 2],
    linear_color: [u8; 8],
    max_pixel_clock: U32,
    reserved: [u8; 190],
}
const _: () = assert!(size_of::<Mode>() == 256 && core::mem::offset_of!(Mode, base) == 40);
impl Mode {
    fn framebuffer(self, version: u16, windows: &[PciWindow]) -> Option<FramebufferInfo> {
        if self.attributes.get() & 0x91 != 0x91 || self.model != 6 || !matches!(self.bpp, 24 | 32) {
            return None;
        }
        let linear = version >= 0x300 && self.linear_stride.get() != 0;
        let stride_bytes = u32::from(if linear {
            self.linear_stride.get()
        } else {
            self.stride.get()
        });
        let width = u32::from(self.width.get());
        let height = u32::from(self.height.get());
        let bytes_per_pixel = u32::from(self.bpp) / 8;
        let size = u64::from(stride_bytes) * u64::from(height);
        let base = u64::from(self.base.get());
        if width < 640
            || height < 400
            || stride_bytes < width * bytes_per_pixel
            || stride_bytes % bytes_per_pixel != 0
            || !windows
                .iter()
                .any(|w| base >= w.base && base + size <= w.base + w.size)
        {
            return None;
        }
        let color = if linear {
            self.linear_color
        } else {
            [
                self.red_size,
                self.red_pos,
                self.green_size,
                self.green_pos,
                self.blue_size,
                self.blue_pos,
                self.reserved_size,
                self.reserved_pos,
            ]
        };
        if [0, 2, 4].into_iter().any(|i| {
            color[i] != 8 || u16::from(color[i + 1]) + u16::from(color[i]) > u16::from(self.bpp)
        }) {
            return None;
        }
        let masks = [1, 3, 5].map(|i| 0xffu32 << color[i]);
        if masks[0] & masks[1] != 0 || masks[0] & masks[2] != 0 || masks[1] & masks[2] != 0 {
            return None;
        }
        Some(FramebufferInfo {
            base_addr: base,
            width,
            height,
            stride: stride_bytes / bytes_per_pixel,
            bits_per_pixel: self.bpp,
            red_size: color[0],
            red_pos: color[1],
            green_size: color[2],
            green_pos: color[3],
            blue_size: color[4],
            blue_pos: color[5],
        })
    }
}
fn far(pointer: u32) -> usize {
    ((pointer >> 16) << 4) as usize + (pointer & 0xffff) as usize
}

/// POST a matching VGA ROM and select a supported linear VBE mode.
///
/// # Safety
/// Ring 0, BSP only, after PCI allocation. The caller exclusively owns the
/// display, legacy VGA/PCI I/O and the supplied identity-mapped BAR/UMA
/// windows. Register MMIO must be UC; prefetchable memory may be WC.
/// The ROM must be a trusted board firmware asset. `rom_device` is
/// its expected PCIR device ID; any alias must be silicon-validated by the
/// platform. Do not retry on failure: the device may be partially programmed.
/// MMIO windows stay mapped.
pub unsafe fn initialize(
    pci: &PciEcam,
    address: PciAddress,
    rom: &[u8],
    rom_device: u16,
    windows: &[PciWindow],
    timer: &dyn Timer,
) -> Result<FramebufferInfo, Error> {
    if BUSY.swap(true, Ordering::Acquire) {
        return Err(Error::Busy);
    }
    let _guard = Guard {
        cf8: Some(PioRegister::<u32>::new(0xcf8).get()),
    };
    let device = pci.device(address).ok_or(Error::Rom)?;
    // The platform may select a silicon-specific compatible ROM alias.
    let rom = rom_image(
        rom,
        u32::from(device.read16(0)) | (u32::from(rom_device) << 16),
    )?;
    if device.read8(0x0b) != 3
        || device.read8(0x0e) & 0x7f != 0
        || windows.iter().any(|w| {
            !matches!(w.kind, fstart_pci::PciWindowKind::Mmio)
                || w.size == 0
                || w.base.checked_add(w.size).is_none()
        })
    {
        return Err(Error::Bounds);
    }
    // SAFETY: a validated, allocated Type-0 VGA function; BSP owns it.
    let config = unsafe { device.regs::<fstart_pci::PciType0Config>() };
    config.command.modify(
        fstart_pci::PCI_COMMAND_BITS::IO_SPACE::SET
            + fstart_pci::PCI_COMMAND_BITS::MEMORY_SPACE::SET
            + fstart_pci::PCI_COMMAND_BITS::BUS_MASTER::SET,
    );
    // BAR allocation does not imply forwarding the fixed VGA apertures.
    // Enable only the ancestor bridges of the selected display function.
    for bridge in pci.devices().filter_map(|a| pci.device(a)) {
        if bridge.read8(0x0e) & 0x7f != 1 {
            continue;
        }
        // SAFETY: the function has a Type-1 header and belongs to this allocator.
        let regs = unsafe { bridge.regs::<fstart_pci::PciType1Config>() };
        if regs.secondary_bus.get() != 0
            && regs.secondary_bus.get() <= address.bus()
            && address.bus() <= regs.subordinate_bus.get()
        {
            regs.command.modify(
                fstart_pci::PCI_COMMAND_BITS::IO_SPACE::SET
                    + fstart_pci::PCI_COMMAND_BITS::MEMORY_SPACE::SET,
            );
            regs.bridge_control
                .modify(fstart_pci::PCI_BRIDGE_CONTROL_BITS::VGA_ENABLE::SET);
        }
    }
    fstart_log::info!(
        "VBIOS: IO BAR {} legacy VGA port {}",
        fstart_log::Hex(device.read32(0x20).into()),
        fstart_log::Hex(PioRegister::<u8>::new(0x3c3).get().into())
    );
    let mut ctx = Context {
        memory: vec![0; GUEST_SIZE],
        pci: Some(pci),
        windows,
        timer,
        started: timer.timestamp_us(),
        steps: 0,
        error: None,
        last_memory: (0, 0),
        last_port: (0, 0),
    };
    ctx.memory[ROM_BASE..ROM_BASE + rom.len()].copy_from_slice(rom);
    ctx.memory[0x10000] = 0xf4; // successful far-return/IRET sentinel
    ctx.memory[0x413..0x415].copy_from_slice(&640u16.to_le_bytes());
    ctx.memory[0xfffd9..0xfffe0].copy_from_slice(b"PCI_ISA");
    ctx.memory[0xffff5..0xffffd].copy_from_slice(b"06/11/99");
    ctx.memory[0xffffe] = 0xfc;
    let machine = Machine::new(ctx)?;
    // SAFETY: the guard clears ACTIVE before releasing exclusive ownership.
    // Machine never moves while this pointer is published.
    ACTIVE.store(machine.context.get().cast(), Ordering::Release);
    let bdf = u32::from(address.bus()) << 8
        | u32::from(address.device()) << 3
        | u32::from(address.function());
    fstart_log::info!("VBIOS: backend Rust rsemu");
    fstart_log::info!(
        "VBIOS: POST {}:{}:{}",
        address.bus(),
        address.device(),
        address.function()
    );
    let completed = machine.post(bdf);
    if let Some(error) = machine.with(|ctx| ctx.error) {
        return Err(error);
    }
    if !completed {
        return Err(Error::Emulation);
    }
    fstart_log::info!(
        "VBIOS: POST complete ({} execution steps)",
        machine.with(|ctx| ctx.steps)
    );
    machine.vbe(0x4f00, 0, 0)?;
    if machine.bytes::<4>(BUFFER).as_ref() != Some(b"VESA") {
        return Err(Error::Vbe);
    }
    let version = U16::read_from_bytes(&machine.bytes::<2>(BUFFER + 4).ok_or(Error::Vbe)?)
        .map_err(|_| Error::Vbe)?
        .get();
    let pointer = U32::read_from_bytes(&machine.bytes::<4>(BUFFER + 14).ok_or(Error::Vbe)?)
        .map_err(|_| Error::Vbe)?
        .get();
    let mut modes = Vec::new();
    for index in 0..512 {
        let start = far(pointer).checked_add(index * 2).ok_or(Error::Vbe)?;
        let bytes = machine.bytes::<2>(start).ok_or(Error::Vbe)?;
        let mode = U16::read_from_bytes(&bytes).map_err(|_| Error::Vbe)?.get();
        if mode == 0xffff {
            break;
        }
        modes.push(mode);
    }
    let preferred = if machine.vbe(0x4f15, 1, 0).is_ok() {
        let bytes = machine.bytes::<128>(BUFFER).ok_or(Error::Vbe)?;
        edid_info::edid::Edid::parse(&bytes).ok().and_then(|edid| {
            edid.base().descriptors().iter().find_map(|d| match d {
                edid_info::base::descriptors::Descriptor::Timing(t) => {
                    Some((t.horizontal().active(), t.vertical().active()))
                }
                _ => None,
            })
        })
    } else {
        None
    };
    fstart_log::info!(
        "VBE: {} modes, version {}",
        modes.len(),
        fstart_log::Hex(version.into())
    );
    let target = preferred.unwrap_or((1024, 768));
    let mut best = None;
    for number in modes {
        if machine.vbe(0x4f01, 0, number.into()).is_err() {
            continue;
        }
        let mode = Mode::read_from_bytes(&machine.bytes::<256>(BUFFER).ok_or(Error::Vbe)?)
            .map_err(|_| Error::Vbe)?;
        let Some(info) = mode.framebuffer(version, windows) else {
            continue;
        };
        let fits = info.width <= u32::from(target.0) && info.height <= u32::from(target.1);
        let exact = info.width == u32::from(target.0) && info.height == u32::from(target.1);
        let score = (exact, fits, info.width * info.height, info.bits_per_pixel);
        if best.as_ref().is_none_or(|(old, _, _)| score > *old) {
            best = Some((score, number, info));
        }
    }
    let (_, number, _) = best.ok_or(Error::Mode)?;
    machine.vbe(0x4f02, u32::from(number) | 0x4000, 0)?;
    machine.vbe(0x4f01, 0, number.into())?;
    let info = Mode::read_from_bytes(&machine.bytes::<256>(BUFFER).ok_or(Error::Vbe)?)
        .map_err(|_| Error::Vbe)?
        .framebuffer(version, windows)
        .ok_or(Error::Mode)?;
    fstart_log::info!(
        "VBE: mode {} {}x{} stride {} bpp {} at {}",
        fstart_log::Hex(number.into()),
        info.width,
        info.height,
        info.stride,
        info.bits_per_pixel,
        fstart_log::Hex(info.base_addr)
    );
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::FromZeros;
    struct Clock;
    impl Timer for Clock {
        fn delay_us(&self, _: u64) {}
        fn timestamp_us(&self) -> u64 {
            0
        }
    }
    #[test]
    fn emulator_far_return_uses_private_ram_and_callbacks_reject_unmapped_memory() {
        assert!(!BUSY.swap(true, Ordering::Acquire));
        let _guard = Guard { cf8: None };
        let machine = Machine::new(Context {
            memory: vec![0; GUEST_SIZE],
            pci: None,
            windows: &[],
            timer: &Clock,
            started: 0,
            steps: 0,
            error: None,
            last_memory: (0, 0),
            last_port: (0, 0),
        })
        .unwrap();
        machine.with(|ctx| {
            ctx.memory[ROM_BASE + 3] = 0xcb;
            ctx.memory[0x10000] = 0xf4;
        });
        ACTIVE.store(machine.context.get().cast(), Ordering::Release);
        assert!(machine.post(0));
        assert!(machine.with(|ctx| ctx.error.is_none() && ctx.steps < 10));
        // A ROM-installed INT10 writes the VBE controller signature through
        // ES:DI, returns AX=004f and IRETs to the private sentinel. Repeat it
        // to exercise waking the core and resetting its private stack.
        machine.with(|ctx| {
            ctx.memory[0x40..0x44].copy_from_slice(&0xc000_0100u32.to_le_bytes());
            let handler = [
                0x26, 0xc7, 0x05, 0x56, 0x45, 0x26, 0xc7, 0x45, 0x02, 0x53, 0x41, 0xb8, 0x4f, 0x00,
                0xcf,
            ];
            ctx.memory[ROM_BASE + 0x100..ROM_BASE + 0x100 + handler.len()]
                .copy_from_slice(&handler);
        });
        for _ in 0..2 {
            let regs = machine.vbe(0x4f00, 0, 0).unwrap();
            assert_eq!(regs.eax as u16, 0x004f);
            assert_eq!(machine.bytes::<4>(BUFFER), Some(*b"VESA"));
        }
        assert_eq!(memory_read(0xffff_ff00, 4), u32::MAX);
        assert_eq!(machine.with(|ctx| ctx.error), Some(Error::Bounds));
    }
    #[test]
    fn legacy_rom_requires_valid_length_identity_and_checksum() {
        let mut rom = vec![0; 512];
        let header = RomHeader {
            signature: U16::new(0xaa55),
            blocks: 1,
            reserved: [0; 21],
            pcir: U16::new(0x20),
        };
        let pcir = Pcir {
            signature: *b"PCIR",
            vendor: U16::new(0x1002),
            device: U16::new(0x15dd),
            reserved: [0; 8],
            blocks: U16::new(1),
            revision: U16::new(0),
            code_type: 0,
            indicator: 0x80,
            tail: U16::new(0),
        };
        rom[..size_of::<RomHeader>()].copy_from_slice(header.as_bytes());
        rom[0x20..0x20 + size_of::<Pcir>()].copy_from_slice(pcir.as_bytes());
        rom[511] = 0u8.wrapping_sub(rom.iter().fold(0u8, |a, &b| a.wrapping_add(b)));
        assert_eq!(rom_image(&rom, 0x15dd1002).map(|r| r.len()), Ok(512));
        assert_eq!(rom_image(&rom, 0x15d81002), Err(Error::Rom));
        rom[511] ^= 1;
        assert_eq!(rom_image(&rom, 0x15dd1002), Err(Error::Rom));
        rom[2] = 2;
        assert_eq!(rom_image(&rom, 0x15dd1002), Err(Error::Rom));
    }
    #[test]
    fn vbe_mode_checks_stride_color_and_allocated_framebuffer_bounds() {
        let mut mode = Mode::new_zeroed();
        mode.attributes.set(0x91);
        mode.model = 6;
        mode.bpp = 32;
        mode.width.set(1366);
        mode.height.set(768);
        mode.linear_stride.set(5632);
        mode.base.set(0xe0000000);
        mode.linear_color = [8, 16, 8, 8, 8, 0, 8, 24];
        let windows = [PciWindow {
            kind: fstart_pci::PciWindowKind::Mmio,
            base: 0xe0000000,
            size: 0x1000000,
            prefetchable: true,
        }];
        let info = mode.framebuffer(0x300, &windows).unwrap();
        assert_eq!((info.width, info.height, info.stride), (1366, 768, 1408));
        mode.linear_color[3] = 16;
        assert!(mode.framebuffer(0x300, &windows).is_none());
        mode.linear_color[3] = 8;
        mode.base.set(0xd0000000);
        assert!(mode.framebuffer(0x300, &windows).is_none());
    }
}
