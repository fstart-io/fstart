//! Allwinner sunxi display framebuffer driver.
//!
//! Initializes the display scanout path for the Allwinner boards fstart
//! supports and exposes the resulting XRGB8888 linear framebuffer via
//! [`fstart_services::Framebuffer`] so CrabEFI can publish it as UEFI GOP.
//!
//! Hardware sequence references:
//! - U-Boot `drivers/video/sunxi/sunxi_display.c` + `lcdc.c` for A20 DE1.
//! - U-Boot `drivers/video/sunxi/sunxi_de2.c` + `sunxi_dw_hdmi.c` for H3/H5.
//! - D1 DE/TCON/HDMI register programming follows the same DE2/DW-HDMI shape;
//!   the D1-specific clock values are taken from the public bare-metal D1/H3
//!   examples because this U-Boot checkout has D1 display DT nodes but no C
//!   display init path.

#![no_std]

use core::ptr;

use serde::{Deserialize, Serialize};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::RegisterLongName;

use fstart_mmio::MmioReadWrite;
use fstart_services::device::{Device, DeviceError};
use fstart_services::framebuffer::{Framebuffer, FramebufferInfo};
use fstart_services::{PostDramInit, ServiceError};

/// Sunxi display controller generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SunxiDisplayGen {
    /// Legacy display engine used by A20/sun7i.
    Sun7iA20,
    /// DE2 display engine used by H3/H5.
    Sun8iH3,
    /// DE2-family display engine used by D1/T113.
    Sun20iD1,
}

/// Output path to initialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SunxiDisplayOutput {
    /// HDMI/DVI output path.
    Hdmi,
    /// RGB/LVDS LCD panel path.
    Lcd,
}

/// Configuration for the Allwinner sunxi display framebuffer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SunxiDisplayConfig {
    /// SoC display generation.
    pub gen: SunxiDisplayGen,
    /// Output path.
    pub output: SunxiDisplayOutput,
    /// CCU base address.
    pub ccu_base: u64,
    /// Display engine base address.
    pub de_base: u64,
    /// LCDC/TCON base address.
    pub tcon_base: u64,
    /// HDMI controller base address, or 0 for panel-only configs.
    pub hdmi_base: u64,
    /// Linear framebuffer base in DRAM.
    pub fb_base: u64,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Stride in pixels. Use 0 to default to `width`.
    pub stride: u32,
    /// DRAM base as seen by legacy DE1 DMA. Use 0 to default to 0x40000000.
    #[serde(default)]
    pub dram_base: u64,
    /// Pixel clock in Hz. Use 0 to select a board/resolution default.
    #[serde(default)]
    pub pixel_clock_hz: u32,
    /// Horizontal front porch in pixels. Use 0 for a default.
    #[serde(default)]
    pub h_front_porch: u32,
    /// Horizontal sync width in pixels. Use 0 for a default.
    #[serde(default)]
    pub h_sync_len: u32,
    /// Horizontal back porch in pixels. Use 0 for a default.
    #[serde(default)]
    pub h_back_porch: u32,
    /// Vertical front porch in lines. Use 0 for a default.
    #[serde(default)]
    pub v_front_porch: u32,
    /// Vertical sync width in lines. Use 0 for a default.
    #[serde(default)]
    pub v_sync_len: u32,
    /// Vertical back porch in lines. Use 0 for a default.
    #[serde(default)]
    pub v_back_porch: u32,
}

#[derive(Clone, Copy)]
struct Mode {
    x: u32,
    y: u32,
    hfp: u32,
    hsync: u32,
    hbp: u32,
    vfp: u32,
    vsync: u32,
    vbp: u32,
    pixel_clock_hz: u32,
}

impl Mode {
    fn htotal(self) -> u32 {
        self.x + self.hfp + self.hsync + self.hbp
    }

    fn vtotal(self) -> u32 {
        self.y + self.vfp + self.vsync + self.vbp
    }
}

tock_registers::register_bitfields![u32,
    CCU_PLL [
        ENABLE OFFSET(31) NUMBITS(1) [],
        LDO_ENABLE OFFSET(25) NUMBITS(1) [],
        OUTPUT_ENABLE OFFSET(24) NUMBITS(1) [],
        N OFFSET(8) NUMBITS(8) [],
        M OFFSET(0) NUMBITS(4) []
    ],
    CCU_GATE [
        ENABLE OFFSET(31) NUMBITS(1) [],
        PARENT_PLL_VIDEO0_4X OFFSET(24) NUMBITS(1) [],
        BE_RESET OFFSET(26) NUMBITS(1) [],
        D1_RESET OFFSET(16) NUMBITS(1) [],
        AHB_BE0 OFFSET(12) NUMBITS(1) [],
        AHB_HDMI OFFSET(11) NUMBITS(1) [],
        BUS_HDMI_MODE OFFSET(10) NUMBITS(2) [],
        AHB_LCD0 OFFSET(4) NUMBITS(1) [],
        AHB_TCON0 OFFSET(3) NUMBITS(1) [],
        D1_GATE OFFSET(0) NUMBITS(1) [],
        DIV OFFSET(0) NUMBITS(4) []
    ],
    DE_CTRL [ ENABLE OFFSET(0) NUMBITS(1) [] ],
    DE1_MODE [ ENABLE OFFSET(0) NUMBITS(1) [], APPLY OFFSET(1) NUMBITS(1) [], LAYER0_ENABLE OFFSET(8) NUMBITS(1) [] ],
    DE1_LAYER [ ENABLE OFFSET(0) NUMBITS(1) [], FORMAT OFFSET(8) NUMBITS(8) [], LINE_WIDTH OFFSET(5) NUMBITS(16) [] ],
    DE2_UI_ATTR [ ENABLE OFFSET(0) NUMBITS(1) [], FORMAT OFFSET(8) NUMBITS(4) [] ],
    TCON_GCTL [ TCON0_ENABLE OFFSET(0) NUMBITS(1) [], GLOBAL_ENABLE OFFSET(31) NUMBITS(1) [] ],
    TCON1_CTL [ ENABLE OFFSET(31) NUMBITS(1) [], CLOCK_DELAY OFFSET(4) NUMBITS(5) [] ],
    TCON_IO_POL [ HSYNC OFFSET(24) NUMBITS(1) [], VSYNC OFFSET(25) NUMBITS(1) [] ],
    TCON_MUX [ CHANNEL OFFSET(0) NUMBITS(4) [] ],
    LEGACY_HDMI_CTRL [ ENABLE OFFSET(31) NUMBITS(1) [], VIDEO_ENABLE OFFSET(30) NUMBITS(1) [] ],
    DW_PHY_CTRL [
        BIT1 OFFSET(1) NUMBITS(1) [], BIT2 OFFSET(2) NUMBITS(1) [], BIT3 OFFSET(3) NUMBITS(1) [],
        RANGE OFFSET(4) NUMBITS(4) [], RANGE2 OFFSET(8) NUMBITS(4) [],
        BIT16 OFFSET(16) NUMBITS(1) [], BIT18 OFFSET(18) NUMBITS(1) [], BIT19 OFFSET(19) NUMBITS(1) [],
        BIT25 OFFSET(25) NUMBITS(1) [], BIT26 OFFSET(26) NUMBITS(1) [], BIT30 OFFSET(30) NUMBITS(1) [], BIT31 OFFSET(31) NUMBITS(1) []
    ],
];

tock_registers::register_bitfields![u8,
    DW_HDMI_VIDEO_CONF [ DATA_ENABLE_POL OFFSET(6) NUMBITS(1) [], VSYNC_POL OFFSET(5) NUMBITS(1) [], HSYNC_POL OFFSET(4) NUMBITS(1) [], INTERLACE OFFSET(3) NUMBITS(1) [] ],
];

tock_registers::register_structs! {
    /// Sunxi CCU registers touched by the display path.
    CcuRegs {
        (0x000 => _reserved_start),
        (0x010 => pll_video0_h3: MmioReadWrite<u32, CCU_PLL::Register>),
        (0x014 => _reserved0),
        (0x040 => pll_video0_d1: MmioReadWrite<u32, CCU_PLL::Register>),
        (0x044 => _reserved1),
        (0x048 => pll_de_h3: MmioReadWrite<u32, CCU_PLL::Register>),
        (0x04c => _reserved2),
        (0x064 => ahb_gate0: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x068 => _reserved3),
        (0x100 => be0_reset: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x104 => de_mod_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x108 => _reserved4),
        (0x118 => lcd0_ch1_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x11c => _reserved5),
        (0x12c => a20_lcd0_ch1_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x130 => _reserved6),
        (0x150 => hdmi_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x154 => hdmi_slow_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x158 => _reserved7),
        (0x2c4 => bus_gate3: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x2c8 => _reserved8),
        (0x600 => d1_de_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x604 => _reserved9),
        (0x60c => d1_de_gate_reset: MmioReadWrite<u32, CCU_GATE::Register>),
        (0x610 => _reserved10),
        (0xb04 => d1_hdmi_24m_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0xb08 => _reserved11),
        (0xb1c => d1_hdmi_gate: MmioReadWrite<u32, CCU_GATE::Register>),
        (0xb20 => _reserved12),
        (0xb80 => d1_tcon_tv_clk: MmioReadWrite<u32, CCU_GATE::Register>),
        (0xb84 => _reserved13),
        (0xb9c => d1_tcon_tv_gate: MmioReadWrite<u32, CCU_GATE::Register>),
        (0xba0 => @END),
    }
}

tock_registers::register_structs! {
    /// Display engine gate/reset block at the DE base.
    DeGateRegs {
        (0x000 => ctrl: MmioReadWrite<u32, DE_CTRL::Register>),
        (0x004 => reset: MmioReadWrite<u32, DE_CTRL::Register>),
        (0x008 => gate: MmioReadWrite<u32, DE_CTRL::Register>),
        (0x00c => _reserved0),
        (0x010 => config: MmioReadWrite<u32, DE_CTRL::Register>),
        (0x014 => @END),
    }
}

tock_registers::register_structs! {
    /// Legacy DE1 backend registers used for A20 scanout.
    De1BackendRegs {
        (0x000 => mode: MmioReadWrite<u32, DE1_MODE::Register>),
        (0x004 => _reserved0),
        (0x008 => disp_size: MmioReadWrite<u32>),
        (0x00c => _reserved1),
        (0x010 => layer_size: MmioReadWrite<u32>),
        (0x014 => _reserved2),
        (0x020 => layer_coord: MmioReadWrite<u32>),
        (0x024 => _reserved3),
        (0x040 => line_width: MmioReadWrite<u32, DE1_LAYER::Register>),
        (0x044 => _reserved4),
        (0x050 => fb_addr_low: MmioReadWrite<u32>),
        (0x054 => _reserved5),
        (0x060 => fb_addr_high: MmioReadWrite<u32>),
        (0x064 => _reserved6),
        (0x070 => attr: MmioReadWrite<u32, DE1_LAYER::Register>),
        (0x074 => _reserved7),
        (0x090 => format: MmioReadWrite<u32, DE1_LAYER::Register>),
        (0x094 => _reserved8),
        (0x0a0 => premultiply: MmioReadWrite<u32>),
        (0x0a4 => _reserved9),
        (0x1c0 => reg_reload: MmioReadWrite<u32>),
        (0x1c4 => @END),
    }
}

tock_registers::register_structs! {
    /// DE2 mixer global registers.
    De2MixerRegs {
        (0x000 => global_ctl: MmioReadWrite<u32>),
        (0x004 => _reserved0),
        (0x008 => reg_reload: MmioReadWrite<u32>),
        (0x00c => size: MmioReadWrite<u32>),
        (0x010 => @END),
    }
}

tock_registers::register_structs! {
    /// DE2 blender registers used for a single UI plane.
    De2BlenderRegs {
        (0x000 => route: MmioReadWrite<u32>),
        (0x004 => _reserved0),
        (0x080 => pipe_ctl: MmioReadWrite<u32>),
        (0x084 => _reserved1),
        (0x088 => fill_color: MmioReadWrite<u32>),
        (0x08c => out_size: MmioReadWrite<u32>),
        (0x090 => blend_mode: MmioReadWrite<u32>),
        (0x094 => @END),
    }
}

tock_registers::register_structs! {
    /// DE2 UI channel registers for the framebuffer plane.
    De2UiRegs {
        (0x000 => attr: MmioReadWrite<u32, DE2_UI_ATTR::Register>),
        (0x004 => size: MmioReadWrite<u32>),
        (0x008 => coord: MmioReadWrite<u32>),
        (0x00c => pitch: MmioReadWrite<u32>),
        (0x010 => top_laddr: MmioReadWrite<u32>),
        (0x014 => _reserved0),
        (0x080 => top_haddr: MmioReadWrite<u32>),
        (0x084 => _reserved1),
        (0x088 => ovl_size: MmioReadWrite<u32>),
        (0x08c => @END),
    }
}

tock_registers::register_structs! {
    /// TCON/LCDC registers used for TV/HDMI timing output.
    TconRegs {
        (0x000 => gctl: MmioReadWrite<u32, TCON_GCTL::Register>),
        (0x004 => gint0: MmioReadWrite<u32>),
        (0x008 => _reserved0),
        (0x044 => tcon0_io_tristate: MmioReadWrite<u32>),
        (0x048 => _reserved1),
        (0x08c => tcon1_io_tristate: MmioReadWrite<u32>),
        (0x090 => tcon1_ctl: MmioReadWrite<u32>),
        (0x094 => tcon1_basic0: MmioReadWrite<u32>),
        (0x098 => tcon1_basic1: MmioReadWrite<u32>),
        (0x09c => tcon1_basic2: MmioReadWrite<u32>),
        (0x0a0 => tcon1_basic3: MmioReadWrite<u32>),
        (0x0a4 => tcon1_basic4: MmioReadWrite<u32>),
        (0x0a8 => tcon1_basic5: MmioReadWrite<u32>),
        (0x0ac => _reserved2),
        (0x0f0 => tcon1_io_pol: MmioReadWrite<u32>),
        (0x0f4 => tcon1_io_tri: MmioReadWrite<u32>),
        (0x0f8 => _reserved3),
        (0x200 => tcon_mux: MmioReadWrite<u32, TCON_MUX::Register>),
        (0x204 => @END),
    }
}

tock_registers::register_structs! {
    /// Legacy A20 HDMI registers used for fixed-mode setup.
    LegacyHdmiRegs {
        (0x000 => _reserved_start),
        (0x004 => ctrl: MmioReadWrite<u32, LEGACY_HDMI_CTRL::Register>),
        (0x008 => irq: MmioReadWrite<u32>),
        (0x00c => _reserved0),
        (0x010 => video_ctrl: MmioReadWrite<u32, LEGACY_HDMI_CTRL::Register>),
        (0x014 => video_size: MmioReadWrite<u32>),
        (0x018 => video_bp: MmioReadWrite<u32>),
        (0x01c => video_fp: MmioReadWrite<u32>),
        (0x020 => video_sync: MmioReadWrite<u32>),
        (0x024 => pll_ctrl: MmioReadWrite<u32>),
        (0x028 => _reserved1),
        (0x200 => phy0: MmioReadWrite<u32>),
        (0x204 => phy1: MmioReadWrite<u32>),
        (0x208 => phy2: MmioReadWrite<u32>),
        (0x20c => phy3: MmioReadWrite<u32>),
        (0x210 => _reserved2),
        (0x2f0 => pad_ctrl0: MmioReadWrite<u32>),
        (0x2f4 => pad_ctrl1: MmioReadWrite<u32>),
        (0x2f8 => _reserved3),
        (0x300 => audio_ctrl: MmioReadWrite<u32>),
        (0x304 => @END),
    }
}

tock_registers::register_structs! {
    /// DW-HDMI PHY registers on DE2-family sunxi SoCs.
    DwHdmiPhyRegs {
        (0x000 => _reserved_start),
        (0x010 => magic0: MmioReadWrite<u32>),
        (0x014 => magic1: MmioReadWrite<u32>),
        (0x018 => _reserved0),
        (0x020 => ctrl: MmioReadWrite<u32, DW_PHY_CTRL::Register>),
        (0x024 => unk24: MmioReadWrite<u32>),
        (0x028 => unk28: MmioReadWrite<u32>),
        (0x02c => unk2c: MmioReadWrite<u32>),
        (0x030 => unk30: MmioReadWrite<u32>),
        (0x034 => unk34: MmioReadWrite<u32>),
        (0x038 => status: MmioReadWrite<u32>),
        (0x03c => unk3c: MmioReadWrite<u32>),
        (0x040 => @END),
    }
}

mod dw_hdmi_ctrl {
    #![allow(clippy::modulo_one)]
    // The DW-HDMI controller exposes byte-addressed registers. tock-registers'
    // layout assertions compute offsets modulo `size_of::<u8>()`, which is
    // necessarily modulo one; this is harmless for an intentionally byte-wide
    // register block and keeps the hardware representation exact.
    use fstart_mmio::MmioReadWrite;

    tock_registers::register_structs! {
        /// Byte-addressed DW-HDMI controller registers used for mode setup.
        pub DwHdmiCtrlRegs {
            (0x0000 => _reserved_start),
                (0x1000 => pub video_conf: MmioReadWrite<u8, super::DW_HDMI_VIDEO_CONF::Register>),
            (0x1001 => pub hactive_lo: MmioReadWrite<u8>),
            (0x1002 => pub hactive_hi: MmioReadWrite<u8>),
            (0x1003 => pub hblank_lo: MmioReadWrite<u8>),
            (0x1004 => pub hblank_hi: MmioReadWrite<u8>),
            (0x1005 => pub vactive_lo: MmioReadWrite<u8>),
            (0x1006 => pub vactive_hi: MmioReadWrite<u8>),
            (0x1007 => pub vblank: MmioReadWrite<u8>),
            (0x1008 => pub hfp_lo: MmioReadWrite<u8>),
            (0x1009 => pub hfp_hi: MmioReadWrite<u8>),
            (0x100a => pub hsync_lo: MmioReadWrite<u8>),
            (0x100b => pub hsync_hi: MmioReadWrite<u8>),
            (0x100c => pub vfp: MmioReadWrite<u8>),
            (0x100d => pub vsync: MmioReadWrite<u8>),
            (0x100e => _reserved0),
            (0x1011 => pub avi0: MmioReadWrite<u8>),
            (0x1012 => pub avi1: MmioReadWrite<u8>),
            (0x1013 => pub avi2: MmioReadWrite<u8>),
            (0x1014 => pub avi3: MmioReadWrite<u8>),
            (0x1015 => pub avi4: MmioReadWrite<u8>),
            (0x1016 => pub avi5: MmioReadWrite<u8>),
            (0x1017 => _reserved1),
            (0x4001 => pub mc_clkdis: MmioReadWrite<u8>),
            (0x4002 => _reserved2),
            (0x4004 => pub phy_mask0: MmioReadWrite<u8>),
            (0x4005 => @END),
        }
    }
}

use dw_hdmi_ctrl::DwHdmiCtrlRegs;

/// Allwinner sunxi display framebuffer device.
pub struct SunxiDisplay {
    config: SunxiDisplayConfig,
}

// SAFETY: the driver only stores physical addresses and uses MMIO/interior
// mutability during single-threaded firmware execution.
unsafe impl Send for SunxiDisplay {}
// SAFETY: see `Send`; shared references are safe because register access is
// synchronized by firmware's single-threaded execution model.
unsafe impl Sync for SunxiDisplay {}

impl Device for SunxiDisplay {
    const NAME: &'static str = "sunxi-display";
    const COMPATIBLE: &'static [&'static str] = &[
        "allwinner,sun7i-a20-display",
        "allwinner,sun8i-h3-display",
        "allwinner,sun20i-d1-display",
    ];

    type Config = SunxiDisplayConfig;

    fn new(config: &Self::Config) -> Result<Self, DeviceError> {
        if config.width == 0 || config.height == 0 {
            return Err(DeviceError::MissingResource("display dimensions"));
        }
        if config.stride != 0 && config.stride < config.width {
            return Err(DeviceError::MissingResource("display stride"));
        }
        Ok(Self { config: *config })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Display scanout is intentionally not programmed from Device::init():
        // bootblocks use DriverInit to bring up storage, but display needs
        // DRAM-backed framebuffer space and is too large/late for SRAM stages.
        // Boards opt in with PostDramInit on the DRAM/UEFI stage.
        Ok(())
    }
}

impl PostDramInit for SunxiDisplay {
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.clear_framebuffer();
        let mode = self.mode();
        match self.config.gen {
            SunxiDisplayGen::Sun7iA20 => self.init_de1_a20(mode),
            SunxiDisplayGen::Sun8iH3 | SunxiDisplayGen::Sun20iD1 => self.init_de2_hdmi(mode),
        }
        Ok(())
    }
}

impl SunxiDisplay {
    fn mode(&self) -> Mode {
        let (pixel, hfp, hsync, hbp, vfp, vsync, vbp) =
            match (self.config.width, self.config.height) {
                (1024, 768) => (65_000_000, 24, 136, 160, 3, 6, 29),
                (1280, 720) => (74_250_000, 110, 40, 220, 5, 5, 20),
                (1920, 1080) => (148_500_000, 88, 44, 148, 4, 5, 36),
                _ => (33_300_000, 40, 48, 40, 13, 3, 29),
            };
        Mode {
            x: self.config.width,
            y: self.config.height,
            hfp: choose(self.config.h_front_porch, hfp),
            hsync: choose(self.config.h_sync_len, hsync),
            hbp: choose(self.config.h_back_porch, hbp),
            vfp: choose(self.config.v_front_porch, vfp),
            vsync: choose(self.config.v_sync_len, vsync),
            vbp: choose(self.config.v_back_porch, vbp),
            pixel_clock_hz: choose(self.config.pixel_clock_hz, pixel),
        }
    }

    fn clear_framebuffer(&self) {
        let stride = self.stride_pixels();
        let bytes = stride.saturating_mul(self.config.height).saturating_mul(4) as usize;
        let fb = self.config.fb_base as *mut u8;
        for offset in (0..bytes).step_by(4) {
            // SAFETY: board config reserves `fb_base..fb_base+bytes` in DRAM
            // for the framebuffer and the firmware runs single-threaded here.
            unsafe { ptr::write_volatile(fb.add(offset) as *mut u32, 0x0000_0000) };
        }
    }

    fn init_de1_a20(&self, mode: Mode) {
        self.init_a20_clocks(mode);
        self.init_de1_backend(mode);
        self.init_tcon1(mode, false);
        if self.config.output == SunxiDisplayOutput::Hdmi && self.config.hdmi_base != 0 {
            self.init_legacy_hdmi(mode);
        }
    }

    fn init_de2_hdmi(&self, mode: Mode) {
        match self.config.gen {
            SunxiDisplayGen::Sun20iD1 => self.init_d1_clocks(mode),
            _ => self.init_de2_clocks(mode),
        }
        self.init_de2_mixer(mode);
        self.init_tcon1(mode, true);
        if self.config.output == SunxiDisplayOutput::Hdmi && self.config.hdmi_base != 0 {
            self.init_dw_hdmi(mode);
        }
    }

    fn init_a20_clocks(&self, _mode: Mode) {
        let ccu = regs::<CcuRegs>(self.config.ccu_base);
        set_reg(
            &ccu.ahb_gate0,
            (CCU_GATE::AHB_BE0::SET + CCU_GATE::AHB_HDMI::SET + CCU_GATE::AHB_LCD0::SET).value,
        );
        set_reg(&ccu.be0_reset, CCU_GATE::BE_RESET::SET.value);
        // U-Boot clock_set_de_mod_clock(): enable BE0 module clock from PLL6_2X.
        ccu.de_mod_clk
            .set((CCU_GATE::ENABLE::SET + CCU_GATE::DIV.val(1)).value);
        // LCD0 CH1 clock gate; parent selection/divisor is intentionally
        // conservative and matches the common 24 MHz oscillator fallback.
        ccu.a20_lcd0_ch1_clk.set(CCU_GATE::ENABLE::SET.value);
        // HDMI and HDMI slow gates.
        set_reg(&ccu.hdmi_clk, CCU_GATE::ENABLE::SET.value);
        set_reg(&ccu.hdmi_slow_clk, CCU_GATE::ENABLE::SET.value);
    }

    fn init_de2_clocks(&self, mode: Mode) {
        let ccu = regs::<CcuRegs>(self.config.ccu_base);
        // H3/H5: PLL_DE = 432 MHz and PLL_VIDEO ~= 297 MHz, matching
        // U-Boot/bare-metal defaults that cover 720p/1080p and divide
        // cleanly for lower modes.
        ccu.pll_de_h3
            .set((CCU_PLL::ENABLE::SET + CCU_PLL::OUTPUT_ENABLE::SET + CCU_PLL::N.val(17)).value);
        ccu.pll_video0_h3.set(
            (CCU_PLL::ENABLE::SET
                + CCU_PLL::LDO_ENABLE::SET
                + CCU_PLL::OUTPUT_ENABLE::SET
                + CCU_PLL::N.val(98)
                + CCU_PLL::M.val(7))
            .value,
        );
        delay(10_000);
        set_reg(
            &ccu.ahb_gate0,
            (CCU_GATE::AHB_BE0::SET + CCU_GATE::AHB_HDMI::SET + CCU_GATE::AHB_TCON0::SET).value,
        );
        set_reg(
            &ccu.bus_gate3,
            (CCU_GATE::AHB_BE0::SET + CCU_GATE::BUS_HDMI_MODE.val(3) + CCU_GATE::AHB_TCON0::SET)
                .value,
        );
        ccu.de_mod_clk
            .set((CCU_GATE::ENABLE::SET + CCU_GATE::PARENT_PLL_VIDEO0_4X::SET).value);
        ccu.hdmi_clk.set(CCU_GATE::ENABLE::SET.value);
        ccu.hdmi_slow_clk.set(CCU_GATE::ENABLE::SET.value);
        let div = (297_000_000u32 / mode.pixel_clock_hz)
            .saturating_sub(1)
            .min(15);
        ccu.lcd0_ch1_clk
            .set((CCU_GATE::ENABLE::SET + CCU_GATE::DIV.val(div)).value);

        let de = regs::<DeGateRegs>(self.config.de_base);
        set_reg(&de.gate, 1);
        set_reg(&de.ctrl, 1);
        set_reg(&de.reset, 1);
        clear_reg(&de.config, 1);
    }

    fn init_d1_clocks(&self, mode: Mode) {
        let ccu = regs::<CcuRegs>(self.config.ccu_base);
        // D1 CCU differs from H3/H5: ccu+0x010 is PLL_DDR0 and must not
        // be touched while running from live DRAM. Use the D1 video PLL and
        // display/TCON/HDMI gate/reset offsets from Linux ccu-sun20i-d1.
        ccu.pll_video0_d1.set(
            (CCU_PLL::ENABLE::SET
                + CCU_PLL::OUTPUT_ENABLE::SET
                + CCU_PLL::N.val(98)
                + CCU_PLL::M.val(7))
            .value,
        );
        delay(10_000);

        // DE module clock: gate, parent PLL_VIDEO0_4X, divide down.
        ccu.d1_de_clk
            .set((CCU_GATE::ENABLE::SET + CCU_GATE::PARENT_PLL_VIDEO0_4X::SET).value);
        set_reg(
            &ccu.d1_de_gate_reset,
            (CCU_GATE::D1_RESET::SET + CCU_GATE::D1_GATE::SET).value,
        );

        // TCON-TV clock and bus gate for HDMI output.
        let div = (297_000_000u32 / mode.pixel_clock_hz)
            .saturating_sub(1)
            .min(15);
        ccu.d1_tcon_tv_clk
            .set((CCU_GATE::ENABLE::SET + CCU_GATE::DIV.val(div)).value);
        set_reg(&ccu.d1_tcon_tv_gate, 1);

        // HDMI 24 MHz and bus gate.
        set_reg(&ccu.d1_hdmi_24m_clk, CCU_GATE::ENABLE::SET.value);
        set_reg(&ccu.d1_hdmi_gate, 1);

        let de = regs::<DeGateRegs>(self.config.de_base);
        set_reg(&de.gate, 1);
        set_reg(&de.ctrl, 1);
        set_reg(&de.reset, 1);
        clear_reg(&de.config, 1);
    }

    fn init_de1_backend(&self, mode: Mode) {
        let be_base = self.config.de_base + 0x800;
        for off in (0..0x800).step_by(4) {
            reg32(be_base + off).set(0);
        }
        let be = regs::<De1BackendRegs>(be_base);
        let wh = wh(mode.x, mode.y);
        be.mode.set(DE1_MODE::ENABLE::SET.value);
        be.disp_size.set(wh);
        be.layer_size.set(wh);
        be.layer_coord.set(0);
        be.line_width
            .set(DE1_LAYER::LINE_WIDTH.val(self.stride_pixels()).value);
        let dram_base = choose64(self.config.dram_base, 0x4000_0000);
        let dma = self.config.fb_base.saturating_sub(dram_base);
        be.fb_addr_low.set((dma << 3) as u32);
        be.fb_addr_high.set((dma >> 29) as u32);
        be.attr.set(0);
        be.format.set(DE1_LAYER::FORMAT.val(0x09).value);
        set_reg(&be.mode, DE1_MODE::LAYER0_ENABLE::SET.value);
        be.reg_reload.set(1);
        set_reg(&be.attr, DE1_LAYER::ENABLE::SET.value);
        set_reg(&be.mode, DE1_MODE::APPLY::SET.value);
    }

    fn init_de2_mixer(&self, mode: Mode) {
        let mux_base = self.config.de_base + 0x100000;
        for off in (0..0x0c000).step_by(4) {
            reg32(mux_base + off).set(0);
        }
        let mux = regs::<De2MixerRegs>(mux_base);
        let size = wh(mode.x, mode.y);
        mux.global_ctl.set(1);
        mux.size.set(size);

        let bld = regs::<De2BlenderRegs>(mux_base + 0x1000);
        bld.route.set(0x100);
        bld.pipe_ctl.set(0x100); // UI channel 1 -> pipe 0.
        bld.fill_color.set(0xff00_0000);
        bld.out_size.set(size);
        bld.blend_mode.set(0x0301);

        let ui = regs::<De2UiRegs>(mux_base + 0x3000);
        ui.attr
            .set((DE2_UI_ATTR::ENABLE::SET + DE2_UI_ATTR::FORMAT.val(4)).value);
        ui.size.set(size);
        ui.coord.set(0);
        ui.pitch.set(self.stride_pixels() * 4);
        ui.top_laddr.set(self.config.fb_base as u32);
        ui.top_haddr.set((self.config.fb_base >> 32) as u32);
        ui.ovl_size.set(size);

        // Disable unused scalers/enhancement units, matching U-Boot.
        for off in [
            0x20000, 0x30000, 0x40000, 0x50000, 0xa0000, 0xa2000, 0xa4000, 0xa6000, 0xa8000,
            0xaa000,
        ] {
            reg32(mux_base + off).set(0);
        }
        mux.reg_reload.set(1);
    }

    fn init_tcon1(&self, mode: Mode, de2: bool) {
        let tcon = regs::<TconRegs>(self.config.tcon_base);
        tcon.gctl.set(0);
        tcon.gint0.set(0);
        tcon.tcon0_io_tristate.set(0);
        tcon.tcon1_io_tristate.set(0xffff_ffff);
        tcon.tcon1_io_tri.set(0xffff_ffff);
        if !de2 {
            set_reg(&tcon.gctl, 1);
        }
        let clk_delay = 30u32;
        tcon.tcon1_ctl
            .set((TCON1_CTL::ENABLE::SET + TCON1_CTL::CLOCK_DELAY.val(clk_delay)).value);
        let size = wh(mode.x, mode.y);
        tcon.tcon1_basic0.set(size);
        tcon.tcon1_basic1.set(size);
        tcon.tcon1_basic2.set(size);
        let htotal = mode.htotal();
        let hbp = mode.hsync + mode.hbp;
        tcon.tcon1_basic3.set(((htotal - 1) << 16) | (hbp - 1));
        let vtotal = mode.vtotal();
        let vbp = mode.vsync + mode.vbp;
        tcon.tcon1_basic4.set((vtotal << 16) | (vbp - 1));
        tcon.tcon1_basic5.set(wh(mode.hsync, mode.vsync));
        tcon.tcon1_io_pol
            .set((TCON_IO_POL::HSYNC::SET + TCON_IO_POL::VSYNC::SET).value);
        tcon.tcon1_io_tri.set(0);
        if de2 {
            clear_reg(&tcon.tcon_mux, TCON_MUX::CHANNEL.mask);
            set_reg(&tcon.tcon_mux, TCON_MUX::CHANNEL.val(1).value);
        }
        set_reg(&tcon.gctl, TCON_GCTL::GLOBAL_ENABLE::SET.value);
    }

    fn init_legacy_hdmi(&self, mode: Mode) {
        let hdmi = regs::<LegacyHdmiRegs>(self.config.hdmi_base);
        hdmi.ctrl.set(LEGACY_HDMI_CTRL::ENABLE::SET.value);
        hdmi.irq.set(0x73);
        hdmi.audio_ctrl.set(0x0800_0000);
        hdmi.pll_ctrl.set(0x03e0_0000);
        hdmi.phy0.set(0x7e80_00ff);
        hdmi.phy1.set(0x00d8_c820);
        hdmi.phy2.set(0xba48_a308);
        hdmi.phy3.set(0);
        hdmi.video_size.set(wh(mode.x, mode.y));
        hdmi.video_bp
            .set(wh(mode.hsync + mode.hbp, mode.vsync + mode.vbp));
        hdmi.video_fp.set(wh(mode.hfp, mode.vfp));
        hdmi.video_sync.set(wh(mode.hsync, mode.vsync));
        hdmi.pad_ctrl0.set(0x0000_0f21);
        hdmi.pad_ctrl1.set(0x0000_000f);
        delay(1000);
        set_reg(
            &hdmi.video_ctrl,
            (LEGACY_HDMI_CTRL::ENABLE::SET + LEGACY_HDMI_CTRL::VIDEO_ENABLE::SET).value,
        );
    }

    fn init_dw_hdmi(&self, mode: Mode) {
        let ctrl = regs::<DwHdmiCtrlRegs>(self.config.hdmi_base);
        let phy = regs::<DwHdmiPhyRegs>(self.config.hdmi_base + 0x10000);
        phy.ctrl.set(0);
        phy.ctrl.set(1);
        delay(500);
        set_reg(
            &phy.ctrl,
            (DW_PHY_CTRL::BIT16::SET + DW_PHY_CTRL::BIT1::SET).value,
        );
        delay(1000);
        set_reg(&phy.ctrl, DW_PHY_CTRL::BIT2::SET.value);
        delay(500);
        set_reg(&phy.ctrl, DW_PHY_CTRL::BIT3::SET.value);
        delay(4000);
        set_reg(&phy.ctrl, DW_PHY_CTRL::BIT19::SET.value);
        delay(10_000);
        set_reg(
            &phy.ctrl,
            (DW_PHY_CTRL::BIT18::SET + DW_PHY_CTRL::RANGE.val(0x7)).value,
        );
        for _ in 0..100_000 {
            if phy.status.get() & 0x80 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        set_reg(
            &phy.ctrl,
            (DW_PHY_CTRL::RANGE.val(0xf) + DW_PHY_CTRL::RANGE2.val(0xf)).value,
        );
        set_reg(
            &phy.unk28,
            (DE_CTRL::ENABLE::SET.value) | DW_PHY_CTRL::BIT2::SET.value,
        );
        clear_reg(&phy.unk2c, DW_PHY_CTRL::BIT26::SET.value);
        phy.unk3c.set(0);
        phy.unk2c.set(0x39dc_5040);
        phy.unk30.set(0x8008_4381);
        delay(10_000);
        phy.unk34.set(1);
        set_reg(&phy.unk2c, DW_PHY_CTRL::BIT25::SET.value);
        delay(10_000);
        let tmp = (phy.status.get() & 0x1f800) >> 11;
        set_reg(
            &phy.unk2c,
            (DW_PHY_CTRL::BIT31::SET + DW_PHY_CTRL::BIT30::SET).value | tmp,
        );
        phy.ctrl.set(0x01ff_ff7f);
        phy.unk24.set(0x8063_a800);
        phy.unk28.set(0x0f81_c485);
        phy.magic0.set(0x5452_4545);
        phy.magic1.set(0x4249_4e47);

        ctrl.video_conf.set(
            (DW_HDMI_VIDEO_CONF::DATA_ENABLE_POL::SET
                + DW_HDMI_VIDEO_CONF::VSYNC_POL::SET
                + DW_HDMI_VIDEO_CONF::HSYNC_POL::SET
                + DW_HDMI_VIDEO_CONF::INTERLACE::SET)
                .value,
        );
        write16_regs(&ctrl.hactive_lo, &ctrl.hactive_hi, mode.x);
        write16_regs(&ctrl.hblank_lo, &ctrl.hblank_hi, mode.htotal() - mode.x);
        write16_regs(&ctrl.vactive_lo, &ctrl.vactive_hi, mode.y);
        ctrl.vblank.set((mode.vtotal() - mode.y) as u8);
        write16_regs(&ctrl.hfp_lo, &ctrl.hfp_hi, mode.hfp);
        write16_regs(&ctrl.hsync_lo, &ctrl.hsync_hi, mode.hsync);
        ctrl.vfp.set(mode.vfp as u8);
        ctrl.vsync.set(mode.vsync as u8);
        ctrl.avi0.set(12);
        ctrl.avi1.set(32);
        ctrl.avi2.set(1);
        ctrl.avi3.set(0x0b);
        ctrl.avi4.set(0x16);
        ctrl.avi5.set(0x21);
        ctrl.phy_mask0.set(0);
        ctrl.mc_clkdis.set(0x74);
    }

    fn stride_pixels(&self) -> u32 {
        if self.config.stride == 0 {
            self.config.width
        } else {
            self.config.stride
        }
    }
}

impl Framebuffer for SunxiDisplay {
    fn info(&self) -> FramebufferInfo {
        FramebufferInfo {
            base_addr: self.config.fb_base,
            width: self.config.width,
            height: self.config.height,
            stride: self.stride_pixels(),
            bits_per_pixel: 32,
            red_pos: 16,
            red_size: 8,
            green_pos: 8,
            green_size: 8,
            blue_pos: 0,
            blue_size: 8,
        }
    }
}

fn choose(value: u32, default: u32) -> u32 {
    if value == 0 {
        default
    } else {
        value
    }
}

fn choose64(value: u64, default: u64) -> u64 {
    if value == 0 {
        default
    } else {
        value
    }
}

fn wh(width: u32, height: u32) -> u32 {
    ((height - 1) << 16) | (width - 1)
}

fn regs<T>(base: u64) -> &'static T {
    // SAFETY: caller passes the base of a valid, mapped MMIO register block
    // matching `T`'s `register_structs!` layout.
    unsafe { &*(base as *const T) }
}

fn reg32(addr: u64) -> &'static MmioReadWrite<u32> {
    // SAFETY: caller passes a valid, mapped, 4-byte-aligned MMIO register
    // address. Used only for repeated sparse clear loops not worth modeling
    // as individual fields.
    unsafe { &*(addr as *const MmioReadWrite<u32>) }
}

fn set_reg<R: RegisterLongName>(reg: &MmioReadWrite<u32, R>, bits: u32) {
    reg.set(reg.get() | bits);
}

fn clear_reg<R: RegisterLongName>(reg: &MmioReadWrite<u32, R>, bits: u32) {
    reg.set(reg.get() & !bits);
}

fn write16_regs(lo: &MmioReadWrite<u8>, hi: &MmioReadWrite<u8>, value: u32) {
    lo.set((value & 0xff) as u8);
    hi.set(((value >> 8) & 0xff) as u8);
}

fn delay(iterations: usize) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}
