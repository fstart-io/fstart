// SPDX-License-Identifier: MIT
//! Native AST2500 host VGA modesetting, without executing a VGA ROM.
//!
//! Adapted from Linux's MIT-licensed drivers/gpu/drm/ast/{ast_mode.c,
//! ast_2500.c,ast_tables.h,ast_vbios.c,ast_reg.h,ast_mm.c} at
//! 2bc105b55a85c9c9990dce9426d399bf70b74282. See LICENSE for their notices.
//! Coreboot's drivers/aspeed/common uses the same native sequence.
//!
//! A running BMC must have initialized its shared DRAM. We deliberately
//! omit Linux's standalone-GPU DRAM training/P2A path: it can reset the
//! BMC, corrupt its memory, and destroy the KVM connection. Only AST2500
//! (PCI revision 0x4x) is supported. A fixed 1024x768@60 XRGB8888 mode is
//! the Linux headless/KVM default; no monitor or EDID is required.

use fstart_core::{
    indexed::Indexed,
    mmio::MmioRegister,
    services::{FramebufferInfo, Timer},
};
use fstart_pci::{PCI_COMMAND_BITS, PciAddress, PciEcam, PciType0Config};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{RegisterLongName, register_bitfields};

/// ASPEED's PCI vendor ID.
pub const ASPEED_VENDOR_ID: u16 = 0x1a03;
/// PCI device ID shared by the AST VGA family; revision selects AST2500.
pub const AST_VGA_DEVICE_ID: u16 = 0x2000;

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const SIZE: u64 = (WIDTH * HEIGHT * 4) as u64;
// VGA register aliases live at BAR1 + 0x380, not the BMC's ARM addresses.
const IO_OFFSET: u64 = 0x380;
const IO_SIZE: u64 = 0x80;
const CRTC: usize = 0x54;
const SEQ: usize = 0x44;
const GR: usize = 0x4e;

register_bitfields![u8,
    /// BAR1+0x3c3, 8-bit VGA Enable register.
    VgaEnable [ ENABLE OFFSET(0) NUMBITS(1) [] ],
    /// Sequencer index 0x01, 8-bit Clocking Mode.
    Clocking [ SCREEN_OFF OFFSET(5) NUMBITS(1) [] ],
    /// CRTC index 0x11, 8-bit vertical sync end and timing protection.
    VSyncEnd [ LOCK OFFSET(7) NUMBITS(1) [] ],
    /// CRTC index 0x17, 8-bit mode control.
    ModeControl [ SYNC_ENABLE OFFSET(7) NUMBITS(1) [] ],
    /// CRTC index 0xa1, 8-bit extended access control.
    Access [
        VGA_IO_OFF OFFSET(1) NUMBITS(1) [],
        MMIO OFFSET(2) NUMBITS(1) []
    ],
    /// CRTC index 0xa0, 8-bit extended memory mode.
    MemoryMode [
        SEGMENTED OFFSET(4) NUMBITS(1) [],
        LINEAR OFFSET(5) NUMBITS(1) [],
        CHAIN4 OFFSET(6) NUMBITS(1) []
    ],
    /// CRTC index 0xa3, 8-bit extended color mode; preserve transmitter bits.
    Color [ FORMAT OFFSET(0) NUMBITS(4) [ Xrgb8888 = 8 ] ],
    /// CRTC index 0xa8, 8-bit extended control.
    Gamma [ ENABLE OFFSET(1) NUMBITS(1) [] ],
    /// CRTC index 0xb6, 8-bit sync/RAMDAC control.
    Sync [
        HSYNC_OFF OFFSET(0) NUMBITS(1) [],
        VSYNC_OFF OFFSET(1) NUMBITS(1) [],
        RAMDAC OFFSET(2) NUMBITS(1) [],
        DAC_A1 OFFSET(5) NUMBITS(1) [],
        PRE_CATCH OFFSET(6) NUMBITS(2) []
    ],
    /// CRTC index 0xcb, 8-bit hardware cursor control.
    Cursor [ ENABLE OFFSET(1) NUMBITS(1) [] ],
    /// CRTC index 0xd0, 8-bit mirror of SCU100, owned by the BMC.
    DramStatus [
        READY OFFSET(6) NUMBITS(1) [],
        BMC OFFSET(7) NUMBITS(1) []
    ],
    /// CRTC index 0xaa, 8-bit VGA memory size (8 MiB shifted by SIZE).
    VramSize [ SIZE OFFSET(0) NUMBITS(2) [] ],
    /// CRTC index 0x99, 8-bit VGA memory reservation.
    VramReserved [ SIZE OFFSET(0) NUMBITS(2) [] ]
];

type Register<R = ()> = Indexed<MmioRegister<u8>, MmioRegister<u8>, R>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    Resources,
    BmcDramNotReady,
}

struct Ast2500 {
    io: usize,
}

impl Ast2500 {
    fn port<R: RegisterLongName>(&self, offset: usize) -> MmioRegister<u8, R> {
        // SAFETY: initialize checked and exclusively owns the mapped BAR1 aliases.
        unsafe { MmioRegister::new(self.io + offset) }
    }

    fn indexed<R: RegisterLongName>(&self, port: usize, index: u8) -> Register<R> {
        Indexed::new(self.port(port), self.port(port + 1), index)
    }

    fn cr<R: RegisterLongName>(&self, index: u8) -> Register<R> {
        self.indexed(CRTC, index)
    }

    fn modeset(&self) {
        // No scratch/DRAM defaults: the BMC's reservation and transmitter
        // state remain intact. Blank first, then the Linux TrueC standard table.
        self.indexed::<Clocking::Register>(SEQ, 1)
            .modify(Clocking::SCREEN_OFF::SET);
        self.cr::<ModeControl::Register>(0x17)
            .modify(ModeControl::SYNC_ENABLE::CLEAR);
        self.cr::<Access::Register>(0xa1)
            .write(Access::MMIO::SET + Access::VGA_IO_OFF::SET);
        self.cr::<VSyncEnd::Register>(0x11)
            .modify(VSyncEnd::LOCK::CLEAR);
        self.port::<()>(0x42).set(0x2f);
        for (index, value) in [(0, 3), (1, 0x21), (2, 0x0f), (3, 0), (4, 0x0e)] {
            self.indexed::<()>(SEQ, index).set(value);
        }
        // ast_tables.h ExtTrueCTable, CRTC 0x00..0x18 (8 bits each).
        const STANDARD: [u8; 25] = [
            0x5f, 0x4f, 0x50, 0x82, 0x54, 0x80, 0x0b, 0x3e, 0, 0x40, 0, 0, 0, 0, 0, 0, 0xea, 0x0c,
            0xdf, 0x28, 0x40, 0xe7, 0x04, 0x23, 0xff,
        ];
        for (index, value) in STANDARD.into_iter().enumerate() {
            // Base and pitch are programmed below; keep scanout disabled.
            if !matches!(index, 0x0c..=0x0d | 0x13) {
                self.cr::<()>(index as u8).set(value);
            }
        }
        // Attribute Controller's shared address/data flip-flop: reset
        // with Input Status 1, then write each address and data byte.
        let _ = self.port::<()>(0x5a).get();
        for index in 0..20u8 {
            self.port::<()>(0x40).set(index);
            self.port::<()>(0x40).set(if index < 16 {
                index
            } else {
                [1, 0, 0, 0][usize::from(index - 16)]
            });
        }
        self.port::<()>(0x40).set(0x14);
        self.port::<()>(0x40).set(0);
        let _ = self.port::<()>(0x5a).get();
        self.port::<()>(0x40).set(0x20);
        for (index, value) in [0, 0, 0, 0, 0, 0, 5, 0x0f, 0xff].into_iter().enumerate() {
            self.indexed::<()>(GR, index as u8).set(value);
        }
        // Encoded from Linux's res_1024x768[0] by ast_set_crtc_reg:
        // H=1024/1048/1184/1344, V=768/771/777/806, no borders/precatch.
        // Each pair is an 8-bit CRTC index/value, including overflow bits.
        for &(index, value) in XGA_CRTC {
            self.cr::<()>(index).set(value);
        }
        self.cr::<()>(0xfc).set(self.cr::<()>(0xfc).get() & 0xfd);
        self.cr::<()>(0xc0).set(0x80); // AST2500 VCLK65 parameter 1, 8 bits.
        self.cr::<()>(0xc1).set(0x64); // AST2500 VCLK65 parameter 2, 8 bits.
        self.cr::<()>(0xbb).set(self.cr::<()>(0xbb).get() & 0x0f); // PLL parameter 3.
        self.cr::<()>(0xa6).set(96); // CRT low memory-request threshold, 8 bits.
        self.cr::<()>(0xa7).set(120); // CRT high memory-request threshold, 8 bits.
        self.port::<()>(0x42).set(0xef); // 65 MHz clock, negative H/V sync.
        self.cr::<MemoryMode::Register>(0xa0)
            .modify(MemoryMode::SEGMENTED::SET + MemoryMode::LINEAR::SET + MemoryMode::CHAIN4::SET);
        self.cr::<Color::Register>(0xa3)
            .modify(Color::FORMAT::Xrgb8888);
        self.cr::<Gamma::Register>(0xa8).modify(Gamma::ENABLE::SET);
        self.cr::<()>(0x8c).set(0x40); // BMC current color mode: 32 bpp.
        self.cr::<()>(0x8d).set(1); // BMC refresh-rate index: 60 Hz.
        self.cr::<()>(0x8e).set(0x31); // BMC mode ID: 1024x768.
        self.cr::<()>(0x91).set(0); // No NewModeInfo extension for XGA.
        self.cr::<()>(0x13).set(0); // Pitch low: 4096/8 = 0x200.
        self.cr::<()>(0xb0).set(2); // Pitch high, 8-bit register.
        // Scanout starts at VRAM offset zero, NOT the host's PCI BAR address.
        for index in [0x0d, 0x0c, 0xaf] {
            self.cr::<()>(index).set(0);
        }
        self.cr::<Cursor::Register>(0xcb)
            .modify(Cursor::ENABLE::CLEAR);
        self.port::<()>(0x46).set(0xff); // 8-bit DAC pixel mask.
        // Identity gamma, as drm_crtc_fill_gamma_888().
        for value in 0..=255u8 {
            self.port::<()>(0x48).set(value);
            let _ = self.port::<()>(0x44).get();
            for _ in 0..3 {
                self.port::<()>(0x49).set(value);
                let _ = self.port::<()>(0x44).get();
            }
        }
        self.cr::<VSyncEnd::Register>(0x11)
            .modify(VSyncEnd::LOCK::SET);
        self.cr::<Sync::Register>(0xb6).modify(
            Sync::HSYNC_OFF::CLEAR
                + Sync::VSYNC_OFF::CLEAR
                + Sync::PRE_CATCH.val(0)
                + Sync::RAMDAC::SET
                + Sync::DAC_A1::SET,
        );
        self.cr::<ModeControl::Register>(0x17)
            .modify(ModeControl::SYNC_ENABLE::SET);
        self.indexed::<Clocking::Register>(SEQ, 1)
            .modify(Clocking::SCREEN_OFF::CLEAR);
    }
}

// ast_set_crtc_reg(), fixed XGA timings; each register is 8 bits wide.
// 00..07: horizontal timing and vertical total/overflow; 09: overflow;
// 10..12: vertical sync/display; 15..16: vertical blank; AC..AE: overflow.
const XGA_CRTC: &[(u8, u8)] = &[
    (0x00, 0xa3),
    (0x01, 0x7f),
    (0x02, 0x7f),
    (0x03, 0x87),
    (0x04, 0x82),
    (0x05, 0x93),
    (0x06, 0x24),
    (0x07, 0xe5),
    (0x09, 0x60),
    (0x10, 0x02),
    (0x11, 0x08),
    (0x12, 0xff),
    (0x15, 0xff),
    (0x16, 0x25),
    (0xac, 0),
    (0xad, 0),
    (0xae, 0x90),
];

fn usable_vram(size: u8, reserved: u8) -> u64 {
    let total = (8u64 << (size & 3)) << 20;
    let reserved = match reserved & 3 {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 4,
    };
    total - (reserved << 20)
}

/// Initialize one enumerated AST2500, preserving the BMC's DRAM and firmware.
///
/// # Safety
/// BSP only, with exclusive access to this GPU's allocated BARs. They must
/// remain identity-mapped as device memory throughout payload execution.
pub unsafe fn initialize(
    pci: &PciEcam,
    address: PciAddress,
    timer: &dyn Timer,
) -> Result<FramebufferInfo, Error> {
    let device = pci.device(address).ok_or(Error::Device)?;
    if device.vendor_id() != ASPEED_VENDOR_ID
        || device.device_id() != AST_VGA_DEVICE_ID
        || device.read8(8) & 0xf0 != 0x40
        || device.read8(0x0b) != 3
    {
        return Err(Error::Device);
    }
    let framebuffer = device.memory_bar(0x10).ok_or(Error::Resources)?;
    let registers = device.memory_bar(0x14).ok_or(Error::Resources)?;
    let covers = |base, size| {
        pci.assigned_memory_bars()
            .any(|(a, w)| a == address && w.base == base && w.size >= size)
    };
    if !covers(framebuffer, SIZE) || !covers(registers, IO_OFFSET + IO_SIZE) {
        return Err(Error::Resources);
    }
    // SAFETY: the device is a validated Type-0 function, exclusively owned.
    let config = unsafe { device.regs::<PciType0Config>() };
    config.command.modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET);
    let ast = Ast2500 {
        io: usize::try_from(registers + IO_OFFSET).map_err(|_| Error::Resources)?,
    };
    if !ast
        .port::<VgaEnable::Register>(0x43)
        .is_set(VgaEnable::ENABLE)
    {
        ast.port::<VgaEnable::Register>(0x43)
            .write(VgaEnable::ENABLE::SET);
        ast.port::<()>(0x42).set(1);
    }
    ast.cr::<()>(0x80).set(0xa8); // 8-bit extended-register unlock key.
    ast.cr::<Access::Register>(0xa1)
        .write(Access::MMIO::SET + Access::VGA_IO_OFF::SET);
    let started = timer.timestamp_us();
    while !ast
        .cr::<DramStatus::Register>(0xd0)
        .is_set(DramStatus::READY)
    {
        if timer.timestamp_us().wrapping_sub(started) >= 1_000_000 {
            return Err(Error::BmcDramNotReady);
        }
        core::hint::spin_loop();
    }
    let available = usable_vram(
        ast.cr::<VramSize::Register>(0xaa).read(VramSize::SIZE),
        ast.cr::<VramReserved::Register>(0x99)
            .read(VramReserved::SIZE),
    );
    if available < SIZE {
        return Err(Error::Resources);
    }
    ast.modeset();
    fstart_log::info!(
        "AST2500: native 1024x768@60 XRGB8888, framebuffer {:#x}",
        framebuffer
    );
    Ok(FramebufferInfo {
        base_addr: framebuffer,
        width: WIDTH,
        height: HEIGHT,
        stride: WIDTH,
        bits_per_pixel: 32,
        red_pos: 16,
        red_size: 8,
        green_pos: 8,
        green_size: 8,
        blue_pos: 0,
        blue_size: 8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_vram_reservations_are_preserved() {
        assert_eq!(usable_vram(0, 0), 8 << 20);
        assert_eq!(usable_vram(1, 3), 12 << 20);
        assert_eq!(usable_vram(2, 2), 30 << 20);
        assert!(usable_vram(0, 3) >= SIZE);
    }
    #[test]
    fn xga_timing_matches_linux_mode() {
        let cr = |index| u16::from(XGA_CRTC.iter().find(|(i, _)| *i == index).unwrap().1);
        assert_eq!((cr(0) + 5) * 8, 1344);
        assert_eq!((cr(1) + 1) * 8, WIDTH as u16);
        let total = cr(6) | ((cr(7) & 1) << 8) | ((cr(7) & 0x20) << 4);
        let display = cr(0x12) | ((cr(7) & 2) << 7) | ((cr(7) & 0x40) << 3);
        assert_eq!(total + 2, 806);
        assert_eq!(display + 1, HEIGHT as u16);
    }
}
