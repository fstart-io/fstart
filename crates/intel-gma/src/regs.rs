//! Typed Intel GMA display register definitions.
//!
//! This module keeps raw MMIO offsets and bit layouts close to typed
//! `tock-registers` definitions. Generation executors should prefer these
//! register blocks/bitfields over ad-hoc integer masks.

use tock_registers::registers::ReadWrite;
use tock_registers::{register_bitfields, register_structs};

register_bitfields! [u32,
    // ---------------------------------------------------------------------
    // Legacy GMCH connector, pipe, plane, scaler, and panel registers.
    // ---------------------------------------------------------------------

    /// Legacy GMCH ADPA VGA/CRT port control register.
    pub ADPA [
        /// Enable VGA DAC output.
        DAC_ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select pipe B instead of pipe A.
        PIPE_SELECT OFFSET(30) NUMBITS(1) [
            PipeA = 0,
            PipeB = 1
        ],
        /// Use VGA hardware sync polarity.
        USE_VGA_HVPOLARITY OFFSET(15) NUMBITS(1) [],
        /// Disable VSYNC output.
        VSYNC_DISABLE OFFSET(11) NUMBITS(1) [],
        /// Disable HSYNC output.
        HSYNC_DISABLE OFFSET(10) NUMBITS(1) [],
        /// Drive VSYNC active high.
        VSYNC_ACTIVE_HIGH OFFSET(4) NUMBITS(1) [],
        /// Drive HSYNC active high.
        HSYNC_ACTIVE_HIGH OFFSET(3) NUMBITS(1) []
    ],

    /// Legacy GMCH LVDS port control register.
    pub LVDS [
        /// Enable LVDS port.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select pipe B instead of pipe A.
        PIPE_SELECT OFFSET(30) NUMBITS(1) [
            PipeA = 0,
            PipeB = 1
        ],
        /// Enable pixel dithering.
        DITHER_EN OFFSET(25) NUMBITS(1) [],
        /// Invert VSYNC polarity.
        VSYNC_POLARITY_INVERT OFFSET(21) NUMBITS(1) [],
        /// Invert HSYNC polarity.
        HSYNC_POLARITY_INVERT OFFSET(20) NUMBITS(1) [],
        /// Power up clock A/data A lanes.
        CLK_A_DATA_A0A2_POWER OFFSET(8) NUMBITS(2) [
            PowerUp = 3
        ],
        /// Power up clock B lanes.
        CLK_B_POWER OFFSET(4) NUMBITS(2) [
            PowerUp = 3
        ],
        /// Power up data B lanes.
        DATA_B0B2_POWER OFFSET(2) NUMBITS(2) [
            PowerUp = 3
        ]
    ],

    /// Legacy GMCH HDMI/SDVO port control register.
    pub GMCH_HDMI [
        /// Enable HDMI/SDVO port.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select pipe B instead of pipe A.
        PIPE_SELECT OFFSET(30) NUMBITS(1) [
            PipeA = 0,
            PipeB = 1
        ],
        /// Color format selector.
        COLOR_FORMAT OFFSET(26) NUMBITS(3) [],
        /// SDVO encoding selector.
        SDVO_ENCODING OFFSET(10) NUMBITS(2) [
            Hdmi = 2
        ],
        /// Select HDMI protocol mode.
        MODE_SELECT_HDMI OFFSET(9) NUMBITS(1) [],
        /// Drive VSYNC active high.
        VSYNC_ACTIVE_HIGH OFFSET(4) NUMBITS(1) [],
        /// Drive HSYNC active high.
        HSYNC_ACTIVE_HIGH OFFSET(3) NUMBITS(1) []
    ],

    /// Legacy GMCH DisplayPort control register.
    pub GMCH_DP [
        /// Enable DisplayPort output.
        DISPLAY_PORT_ENABLE OFFSET(31) NUMBITS(1) [],
        /// Link-training pattern selector.
        LINK_TRAIN OFFSET(28) NUMBITS(2) [
            Pattern1 = 0,
            Pattern2 = 1,
            Idle = 2,
            Normal = 3
        ],
        /// Voltage swing level.
        VSWING_LEVEL_SET OFFSET(25) NUMBITS(3) [],
        /// Pre-emphasis level.
        PREEMPH_LEVEL_SET OFFSET(22) NUMBITS(3) [],
        /// Encoded lane count minus one.
        PORT_WIDTH OFFSET(19) NUMBITS(3) [],
        /// Enhanced framing enable.
        ENHANCED_FRAMING_ENABLE OFFSET(18) NUMBITS(1) [],
        /// Select pipe B instead of pipe A.
        PIPE_SELECT OFFSET(30) NUMBITS(1) [
            PipeA = 0,
            PipeB = 1
        ],
        /// Limited color range.
        COLOR_RANGE_16_235 OFFSET(8) NUMBITS(1) [],
        /// Drive VSYNC active high.
        VSYNC_ACTIVE_HIGH OFFSET(4) NUMBITS(1) [],
        /// Drive HSYNC active high.
        HSYNC_ACTIVE_HIGH OFFSET(3) NUMBITS(1) []
    ],

    /// Legacy GMCH CLKCFG register.
    pub GMCH_CLKCFG [
        /// Front-side bus frequency selector used to derive raw display clock.
        FSB_FREQ_SEL OFFSET(0) NUMBITS(3) []
    ],

    /// Legacy GMCH HPLLVCO register.
    pub GMCH_HPLLVCO [
        /// VCO selector for desktop G45.
        SELECTOR OFFSET(0) NUMBITS(3) [],
        /// Mobile mirror selector byte, reached through an aligned dword read.
        MOBILE_SELECTOR OFFSET(24) NUMBITS(3) []
    ],

    /// Legacy GMCH timing range register payload.
    pub PIPE_RANGE [
        /// Upper/most-significant timing field, encoded as value minus one.
        HIGH_MINUS_ONE OFFSET(16) NUMBITS(16) [],
        /// Lower/least-significant timing field, encoded as value minus one.
        LOW_MINUS_ONE OFFSET(0) NUMBITS(16) []
    ],

    /// Legacy GMCH pipe configuration register.
    pub PIPECONF [
        /// Pipe enable request.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Hardware pipe enabled status.
        ENABLED_STATUS OFFSET(30) NUMBITS(1) [],
        /// Bits-per-component selector. Encoding is not numeric order:
        /// 8bpc = 0, 10bpc = 1, 6bpc = 2, 12bpc = 3 (libgfxinit
        /// `TRANS_CONF_BPC`, Linux `TRANSCONF_BPC_*`).
        BPC OFFSET(5) NUMBITS(3) [
            Bits8 = 0,
            Bits10 = 1,
            Bits6 = 2,
            Bits12 = 3
        ],
        /// Pipe dither enable.
        DITHER OFFSET(4) NUMBITS(1) []
    ],

    /// Legacy GMCH primary display plane control.
    pub DSPCNTR [
        /// Plane enable bit.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Pixel format selector.
        FORMAT OFFSET(26) NUMBITS(4) [
            Xrgb8888 = 6
        ],
        /// Pipe routed to this plane.
        PIPE_SELECT OFFSET(24) NUMBITS(1) [
            PipeA = 0,
            PipeB = 1
        ],
        /// X-tiled framebuffer surface enable.
        TILED_SURFACE OFFSET(10) NUMBITS(1) []
    ],

    /// Legacy plane size register payload.
    pub PLANE_SIZE [
        /// Height field, encoded as value minus one.
        HEIGHT_MINUS_ONE OFFSET(16) NUMBITS(16) [],
        /// Width field, encoded as value minus one.
        WIDTH_MINUS_ONE OFFSET(0) NUMBITS(16) []
    ],

    /// Legacy plane tile offset register payload.
    pub DSPTILEOFF [
        /// Y tile/linear start coordinate.
        START_Y OFFSET(16) NUMBITS(16) [],
        /// X tile/linear start coordinate.
        START_X OFFSET(0) NUMBITS(16) []
    ],

    /// Legacy VGA control register.
    pub VGACNTRL [
        /// Disable legacy VGA display generation.
        VGA_DISPLAY_DISABLE OFFSET(31) NUMBITS(1) []
    ],

    /// Legacy GMCH panel fitter control.
    pub PFIT_CONTROL [
        /// Enable panel fitter.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select pipe for i965+/G45 panel fitter programming.
        PIPE_SELECT OFFSET(29) NUMBITS(2) [
            PipeA = 0,
            PipeB = 1,
            PipeC = 2
        ],
        /// Pre-i965/G45 scaling mode selector.
        SCALING_MODE OFFSET(26) NUMBITS(2) [
            Auto = 0,
            Pillarbox = 2,
            Letterbox = 3
        ],
        /// Pre-i965 vertical bilinear interpolation enable.
        VERT_INTERP_BILINEAR OFFSET(10) NUMBITS(1) [],
        /// Pre-i965 vertical auto-scale enable.
        VERT_AUTO_SCALE OFFSET(9) NUMBITS(1) [],
        /// Pre-i965 horizontal bilinear interpolation enable.
        HORIZ_INTERP_BILINEAR OFFSET(6) NUMBITS(1) [],
        /// Pre-i965 horizontal auto-scale enable.
        HORIZ_AUTO_SCALE OFFSET(5) NUMBITS(1) [],
        /// Pre-i965 8-to-6 panel dither enable.
        PANEL_8TO6_DITHER_ENABLE OFFSET(3) NUMBITS(1) []
    ],

    /// Legacy panel fitter programmed-ratio register.
    pub PFIT_PGM_RATIOS [
        /// i965+ vertical ratio field.
        I965_VERTICAL OFFSET(16) NUMBITS(13) [],
        /// i965+ horizontal ratio field.
        I965_HORIZONTAL OFFSET(0) NUMBITS(13) [],
        /// Pre-i965 vertical ratio field.
        PRE_I965_VERTICAL OFFSET(20) NUMBITS(12) [],
        /// Pre-i965 horizontal ratio field.
        PRE_I965_HORIZONTAL OFFSET(4) NUMBITS(12) []
    ],

    /// Ironlake/PCH panel fitter control.
    pub PF_CTL [
        /// Enable panel fitter.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Pipe select field used by split-PCH fitter variants with explicit pipe select.
        PIPE_SELECT OFFSET(29) NUMBITS(2) [
            PipeA = 0,
            PipeB = 1,
            PipeC = 2
        ],
        /// Medium filter selection used by libgfxinit/Linux.
        FILTER_MED OFFSET(23) NUMBITS(1) []
    ],

    /// Ironlake/PCH panel fitter window position/size payload.
    pub PF_WIN [
        /// X or width field.
        X_OR_WIDTH OFFSET(16) NUMBITS(16) [],
        /// Y or height field.
        Y_OR_HEIGHT OFFSET(0) NUMBITS(16) []
    ],

    /// Legacy panel power status.
    pub PP_STATUS [
        /// Panel power is on.
        ON OFFSET(31) NUMBITS(1) [],
        /// Current panel power sequencer state.
        SEQUENCE OFFSET(28) NUMBITS(2) []
    ],

    /// Legacy panel power control.
    pub PP_CONTROL [
        /// Write-protect key field.
        UNLOCK_KEY OFFSET(16) NUMBITS(16) [],
        /// Broxton-style power-cycle delay field.
        PWR_CYC_DELAY OFFSET(4) NUMBITS(5) [],
        /// VDD override.
        VDD_OVERRIDE OFFSET(3) NUMBITS(1) [],
        /// Backlight enable.
        BACKLIGHT_ENABLE OFFSET(2) NUMBITS(1) [],
        /// Power down on reset.
        POWER_DOWN_ON_RESET OFFSET(1) NUMBITS(1) [],
        /// Target panel power on.
        TARGET_ON OFFSET(0) NUMBITS(1) []
    ],

    /// Panel power on-delay register.
    pub PP_ON_DELAYS [
        /// PCH panel power port select.
        PORT_SELECT OFFSET(30) NUMBITS(2) [],
        /// Delay from target-on to power-up completion in 100us units.
        PWR_UP OFFSET(16) NUMBITS(13) [],
        /// Delay from power-up to backlight-on in 100us units.
        PWR_UP_TO_BL_ON OFFSET(0) NUMBITS(13) []
    ],

    /// Panel power off-delay register.
    pub PP_OFF_DELAYS [
        /// Delay from target-off to power-down completion in 100us units.
        PWR_DOWN OFFSET(16) NUMBITS(13) [],
        /// Delay from backlight-off to power-down in 100us units.
        BL_OFF_TO_PWR_DOWN OFFSET(0) NUMBITS(13) []
    ],

    /// Panel power divisor/cycle-delay register.
    pub PP_DIVISOR [
        /// Reference divider (24 bits, units of 100 us).
        REF_DIVIDER OFFSET(8) NUMBITS(24) [],
        /// Power-cycle delay field.
        PWR_CYC_DELAY OFFSET(0) NUMBITS(5) []
    ],

    /// Legacy CPU backlight PWM data/control register.
    pub CPU_BLC_PWM_CTL [
        /// Backlight duty cycle field.
        BL_DUTY_CYCLE OFFSET(0) NUMBITS(16) []
    ],

    /// Newer PWM control register.
    pub BXT_BLC_PWM_CTL [
        /// PWM controller enable.
        ENABLE OFFSET(31) NUMBITS(1) []
    ],

    // ---------------------------------------------------------------------
    // Broxton port PLL and PHY helper registers.
    // ---------------------------------------------------------------------

    /// Broxton port PLL enable register.
    pub BXT_PORT_PLL_ENABLE [
        /// Enable the selected port PLL.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Reference clock select bit used by libgfxinit before programming.
        REF_SEL OFFSET(27) NUMBITS(1) []
    ],

    /// Broxton port PLL EBB0 divider register.
    pub BXT_PORT_PLL_EBB0 [
        /// P1 divider.
        P1 OFFSET(13) NUMBITS(3) [],
        /// P2 divider.
        P2 OFFSET(8) NUMBITS(5) []
    ],

    /// Broxton port PLL EBB4 control register.
    pub BXT_PORT_PLL_EBB4 [
        /// Recalibrate request.
        RECALIBRATE OFFSET(14) NUMBITS(1) [],
        /// 10-bit clock enable.
        CLK_10BIT_ENABLE OFFSET(13) NUMBITS(1) []
    ],

    /// Broxton port PLL integer M2 register.
    pub BXT_PORT_PLL0 [
        /// Integer part of fixed-point M2.
        M2_INT OFFSET(0) NUMBITS(8) []
    ],

    /// Broxton port PLL N divider register.
    pub BXT_PORT_PLL1 [
        /// N divider.
        N OFFSET(8) NUMBITS(4) []
    ],

    /// Broxton port PLL fractional M2 register.
    pub BXT_PORT_PLL2 [
        /// Fractional part of fixed-point M2.
        M2_FRAC OFFSET(0) NUMBITS(22) []
    ],

    /// Broxton port PLL fractional-enable register.
    pub BXT_PORT_PLL3 [
        /// Enable fractional M2.
        M2_FRAC_ENABLE OFFSET(16) NUMBITS(1) []
    ],

    /// Broxton port PLL gain coefficient register.
    pub BXT_PORT_PLL6 [
        /// Proportional gain coefficient.
        GAIN_CTL OFFSET(16) NUMBITS(3) [],
        /// Integral gain coefficient.
        INT_COEFF OFFSET(8) NUMBITS(5) [],
        /// Gain coefficient.
        PROP_COEFF OFFSET(0) NUMBITS(4) []
    ],

    /// Broxton port PLL target-count register.
    pub BXT_PORT_PLL8 [
        /// Target count.
        TARGET_CNT OFFSET(0) NUMBITS(10) []
    ],

    /// Broxton port PLL lock-threshold register.
    pub BXT_PORT_PLL9 [
        /// Lock threshold.
        LOCK_THRESHOLD OFFSET(1) NUMBITS(3) []
    ],

    /// Broxton port PLL DCO amplitude register.
    pub BXT_PORT_PLL10 [
        /// DCO amplitude override enable.
        DCO_AMP_OVR_EN_H OFFSET(27) NUMBITS(1) [],
        /// DCO amplitude.
        DCO_AMP OFFSET(10) NUMBITS(4) []
    ],

    /// Broxton PCS lane-stagger register.
    pub BXT_PORT_PCS_DW12 [
        /// Strap override enable.
        LANE_STAGGER_STRAP_OVRD OFFSET(6) NUMBITS(1) [],
        /// Lane-stagger value.
        LANE_STAGGER OFFSET(0) NUMBITS(5) []
    ],

    // ---------------------------------------------------------------------
    // Legacy GMCH PLL, GTT, fence, and hotplug registers.
    // ---------------------------------------------------------------------

    /// Legacy GMCH DPLL control register.
    pub DPLL [
        /// DPLL VCO enable.
        VCO_ENABLE OFFSET(31) NUMBITS(1) [],
        /// High-speed mode for non-LVDS digital outputs.
        HIGH_SPEED OFFSET(30) NUMBITS(1) [],
        /// Disable VGA mode.
        VGA_MODE_DIS OFFSET(28) NUMBITS(1) [],
        /// Output mode selector.
        MODE OFFSET(26) NUMBITS(2) [
            Dac = 1,
            Lvds = 2
        ],
        /// P2 divider selector for 5/7 versus 10/14 encodings.
        P2_5_OR_7 OFFSET(24) NUMBITS(1) [],
        /// Legacy P1 divider bitfield.
        P1_DIVIDER OFFSET(16) NUMBITS(8) [],
        /// Pineview P1 divider bitfield.
        PINEVIEW_P1_DIVIDER OFFSET(15) NUMBITS(8) [],
        /// Reference clock selector.
        REFCLK OFFSET(13) NUMBITS(2) [
            Dref = 0,
            Sdvo = 2,
            Ssc = 3
        ],
        /// Pulse phase selector.
        PULSE_PHASE OFFSET(9) NUMBITS(4) [
            Phase6 = 6
        ]
    ],

    /// Legacy GMCH FP0/FP1 divider register.
    pub FP [
        /// N divider encoding.
        N OFFSET(16) NUMBITS(16) [],
        /// M1 divider encoding.
        M1 OFFSET(8) NUMBITS(8) [],
        /// M2 divider encoding.
        M2 OFFSET(0) NUMBITS(8) []
    ],

    /// Legacy GTT PTE entry.
    pub GTT_PTE [
        /// Physical page frame number.
        ADDR OFFSET(12) NUMBITS(20) [],
        /// Entry valid bit.
        VALID OFFSET(0) NUMBITS(1) []
    ],

    /// Legacy fence lower register.
    pub FENCE_LOWER [
        /// First fenced GTT page.
        PAGE OFFSET(12) NUMBITS(20) [],
        /// Y-major tile walk for Y-tiled surfaces.
        TILE_WALK_YMAJOR OFFSET(1) NUMBITS(1) [],
        /// Fence valid bit.
        VALID OFFSET(0) NUMBITS(1) []
    ],

    /// Legacy fence upper register.
    pub FENCE_UPPER [
        /// Last fenced GTT page.
        PAGE OFFSET(12) NUMBITS(20) [],
        /// Encoded fence pitch field.
        PITCH OFFSET(0) NUMBITS(12) []
    ],

    /// GFX flush control register.
    pub GFX_FLSH_CNTL [
        /// Raw control bits; writing zero flushes legacy pending GTT state.
        RAW OFFSET(0) NUMBITS(32) []
    ],

    /// Legacy port hotplug enable register.
    pub PORT_HOTPLUG_EN [
        PORTB_HOTPLUG_INT_EN OFFSET(29) NUMBITS(1) [],
        PORTC_HOTPLUG_INT_EN OFFSET(28) NUMBITS(1) [],
        PORTD_HOTPLUG_INT_EN OFFSET(27) NUMBITS(1) [],
        SDVOB_HOTPLUG_INT_EN OFFSET(26) NUMBITS(1) [],
        SDVOC_HOTPLUG_INT_EN OFFSET(25) NUMBITS(1) [],
        CRT_HOTPLUG_INT_EN OFFSET(9) NUMBITS(1) [],
        CRT_HOTPLUG_ACTIVATION_PERIOD_64 OFFSET(8) NUMBITS(1) []
    ],

    /// Legacy port hotplug status register.
    pub PORT_HOTPLUG_STAT [
        /// Generic detected bit used by GMCH port control helpers.
        PORT_DETECTED OFFSET(2) NUMBITS(1) [],
        /// DisplayPort-B hotplug status field.
        PORTB_HOTPLUG_STATUS OFFSET(17) NUMBITS(2) [],
        /// DisplayPort-C hotplug status field.
        PORTC_HOTPLUG_STATUS OFFSET(19) NUMBITS(2) [],
        /// DisplayPort-D hotplug status field.
        PORTD_HOTPLUG_STATUS OFFSET(21) NUMBITS(2) [],
        /// SDVO/HDMI-C hotplug status bit.
        SDVOC_HOTPLUG_STATUS OFFSET(3) NUMBITS(1) [],
        /// CRT hotplug status bit.
        CRT_HOTPLUG_STATUS OFFSET(11) NUMBITS(1) []
    ],

    // ---------------------------------------------------------------------
    // Split-PCH connector, PLL, transcoder, and FDI registers.
    // ---------------------------------------------------------------------

    /// Split-PCH ADPA VGA/CRT port control register.
    pub PCH_ADPA [
        /// Enable VGA DAC output.
        DAC_ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select transcoder B instead of transcoder A.
        TRANSCODER_SELECT OFFSET(30) NUMBITS(1) [
            TranscoderA = 0,
            TranscoderB = 1
        ],
        /// Disable VSYNC output.
        VSYNC_DISABLE OFFSET(11) NUMBITS(1) [],
        /// Disable HSYNC output.
        HSYNC_DISABLE OFFSET(10) NUMBITS(1) [],
        /// Drive VSYNC active high.
        VSYNC_ACTIVE_HIGH OFFSET(4) NUMBITS(1) [],
        /// Drive HSYNC active high.
        HSYNC_ACTIVE_HIGH OFFSET(3) NUMBITS(1) []
    ],

    /// Split-PCH LVDS port control register.
    pub PCH_LVDS [
        /// Enable LVDS port.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select transcoder B instead of transcoder A.
        TRANSCODER_SELECT OFFSET(30) NUMBITS(1) [
            TranscoderA = 0,
            TranscoderB = 1
        ],
        /// Invert VSYNC polarity.
        VSYNC_POLARITY_INVERT OFFSET(21) NUMBITS(1) [],
        /// Invert HSYNC polarity.
        HSYNC_POLARITY_INVERT OFFSET(20) NUMBITS(1) [],
        /// Power up clock A/data A lanes.
        CLK_A_DATA_A0A2_POWER OFFSET(8) NUMBITS(2) [
            PowerUp = 3
        ],
        /// Power up clock B lanes.
        CLK_B_POWER OFFSET(4) NUMBITS(2) [
            PowerUp = 3
        ],
        /// Power up data B lanes.
        DATA_B0B2_POWER OFFSET(2) NUMBITS(2) [
            PowerUp = 3
        ]
    ],

    /// Split-PCH HDMI/SDVO port control register.
    pub PCH_HDMI [
        /// Enable HDMI/SDVO port.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select transcoder B instead of transcoder A.
        TRANSCODER_SELECT OFFSET(30) NUMBITS(1) [
            TranscoderA = 0,
            TranscoderB = 1
        ],
        /// Color format selector.
        COLOR_FORMAT OFFSET(26) NUMBITS(3) [],
        /// SDVO encoding selector.
        SDVO_ENCODING OFFSET(10) NUMBITS(2) [
            Hdmi = 2
        ],
        /// Drive VSYNC active high.
        VSYNC_ACTIVE_HIGH OFFSET(4) NUMBITS(1) [],
        /// Drive HSYNC active high.
        HSYNC_ACTIVE_HIGH OFFSET(3) NUMBITS(1) []
    ],

    /// Split-PCH DPLL control register.
    pub PCH_DPLL [
        /// DPLL VCO enable.
        VCO_ENABLE OFFSET(31) NUMBITS(1) [],
        /// High-speed mode for VGA/HDMI/DP style outputs.
        HIGH_SPEED OFFSET(30) NUMBITS(1) [],
        /// Output mode selector.
        MODE OFFSET(26) NUMBITS(2) [
            Dac = 1,
            Lvds = 2
        ],
        /// P2 divider selector for 5/7 versus 10/14 encodings.
        P2_5_OR_7 OFFSET(24) NUMBITS(1) [],
        /// P1 divider bitfield.
        P1_DIVIDER OFFSET(16) NUMBITS(8) [],
        /// Reference clock selector.
        REFCLK OFFSET(13) NUMBITS(2) [
            Dref = 0,
            Sdvo = 2,
            Ssc = 3
        ]
    ],

    /// Split-PCH FP divider register.
    pub PCH_FP [
        /// N divider encoding.
        N OFFSET(16) NUMBITS(16) [],
        /// M1 divider encoding.
        M1 OFFSET(8) NUMBITS(8) [],
        /// M2 divider encoding.
        M2 OFFSET(0) NUMBITS(8) []
    ],

    /// Split-PCH DPLL_SEL register.
    pub PCH_DPLL_SEL [
        /// Enable transcoder C DPLL routing.
        TRANSCODER_C_ENABLE OFFSET(11) NUMBITS(1) [],
        /// PLL select for transcoder C.
        TRANSCODER_C_PLL OFFSET(8) NUMBITS(1) [],
        /// Enable transcoder B DPLL routing.
        TRANSCODER_B_ENABLE OFFSET(7) NUMBITS(1) [],
        /// PLL select for transcoder B.
        TRANSCODER_B_PLL OFFSET(4) NUMBITS(1) [],
        /// Enable transcoder A DPLL routing.
        TRANSCODER_A_ENABLE OFFSET(3) NUMBITS(1) [],
        /// PLL select for transcoder A.
        TRANSCODER_A_PLL OFFSET(0) NUMBITS(1) []
    ],

    /// Split-PCH transcoder config register.
    pub TRANS_CONF [
        /// Enable PCH transcoder.
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Transcoder running state, cleared asynchronously after disable.
        TRANSCODER_STATE OFFSET(30) NUMBITS(1) []
    ],

    /// Ironlake CPU eDP DisplayPort control register (`DP_CTL_A`).
    pub CPU_DP_CTL [
        /// Enable DisplayPort output.
        DISPLAYPORT_ENABLE OFFSET(31) NUMBITS(1) [],
        /// Select pipe B instead of pipe A.
        PIPE_SELECT OFFSET(30) NUMBITS(1) [
            PipeA = 0,
            PipeB = 1
        ],
        /// Encoded lane count minus one.
        PORT_WIDTH OFFSET(19) NUMBITS(3) [],
        /// Enhanced framing enable.
        ENHANCED_FRAMING_ENABLE OFFSET(18) NUMBITS(1) [],
        /// Link PLL frequency selector.
        PLL_FREQUENCY OFFSET(16) NUMBITS(2) [
            Rate270MHz = 0,
            Rate162MHz = 1
        ],
        /// Link PLL enable.
        PLL_ENABLE OFFSET(14) NUMBITS(1) [],
        /// Link-training pattern selector.
        LINK_TRAIN OFFSET(8) NUMBITS(2) [
            Pattern1 = 0,
            Pattern2 = 1,
            Idle = 2,
            Normal = 3
        ],
        /// Drive VSYNC active high.
        VSYNC_ACTIVE_HIGH OFFSET(4) NUMBITS(1) [],
        /// Drive HSYNC active high.
        HSYNC_ACTIVE_HIGH OFFSET(3) NUMBITS(1) []
    ],

    /// FDI TX control register.
    pub FDI_TX_CTL [
        /// Enable FDI transmitter.
        FDI_TX_ENABLE OFFSET(31) NUMBITS(1) [],
        /// Voltage/pre-emphasis training value field.
        VP OFFSET(22) NUMBITS(6) [],
        /// Encoded FDI lane count minus one.
        PORT_WIDTH_SEL OFFSET(19) NUMBITS(3) [],
        /// Enable enhanced framing.
        ENHANCED_FRAMING_ENABLE OFFSET(18) NUMBITS(1) [],
        /// Enable FDI PLL.
        FDI_PLL_ENABLE OFFSET(14) NUMBITS(1) [],
        /// Select composite sync.
        COMPOSITE_SYNC_SELECT OFFSET(11) NUMBITS(1) [],
        /// Enable hardware auto-training.
        AUTO_TRAIN_ENABLE OFFSET(10) NUMBITS(1) [],
        /// Training pattern field used by newer split-PCH generations.
        TRAINING_PATTERN_NEW OFFSET(8) NUMBITS(2) [
            Tp1 = 0,
            Tp2 = 1,
            Idle = 2,
            None = 3
        ],
        /// Hardware auto-training done status.
        AUTO_TRAIN_DONE OFFSET(1) NUMBITS(1) [],
        /// Training pattern field used by older Ironlake-style encodings.
        TRAINING_PATTERN_OLD OFFSET(28) NUMBITS(2) [
            Tp1 = 0,
            Tp2 = 1,
            Idle = 2,
            None = 3
        ]
    ],

    /// FDI RX control register.
    pub FDI_RX_CTL [
        /// Enable FDI receiver.
        FDI_RX_ENABLE OFFSET(31) NUMBITS(1) [],
        /// Frame-start error correction enable.
        FS_ERROR_CORRECTION_ENABLE OFFSET(27) NUMBITS(1) [],
        /// Frame-end error correction enable.
        FE_ERROR_CORRECTION_ENABLE OFFSET(26) NUMBITS(1) [],
        /// Encoded FDI lane count minus one.
        PORT_WIDTH_SEL OFFSET(19) NUMBITS(3) [],
        /// Bits-per-component selector.
        BPC OFFSET(16) NUMBITS(2) [],
        /// Enable FDI PLL.
        FDI_PLL_ENABLE OFFSET(13) NUMBITS(1) [],
        /// Select composite sync.
        COMPOSITE_SYNC_SELECT OFFSET(11) NUMBITS(1) [],
        /// Enable hardware auto-training.
        FDI_AUTO_TRAIN OFFSET(10) NUMBITS(1) [],
        /// Training pattern field used by newer split-PCH generations.
        TRAINING_PATTERN_NEW OFFSET(8) NUMBITS(2) [
            Tp1 = 0,
            Tp2 = 1,
            Idle = 2,
            None = 3
        ],
        /// Enable enhanced framing.
        ENHANCED_FRAMING_ENABLE OFFSET(6) NUMBITS(1) [],
        /// Select rawclk to PCD clock path.
        RAWCLK_TO_PCDCLK_SEL OFFSET(4) NUMBITS(1) [],
        /// Training pattern field used by older Ironlake-style encodings.
        TRAINING_PATTERN_OLD OFFSET(28) NUMBITS(2) [
            Tp1 = 0,
            Tp2 = 1,
            Idle = 2,
            None = 3
        ]
    ],

    /// FDI RX misc register.
    pub FDI_RX_MISC [
        /// Lane 1 power-down encoding.
        FDI_RX_PWRDN_LANE1 OFFSET(26) NUMBITS(2) [],
        /// Lane 0 power-down encoding.
        FDI_RX_PWRDN_LANE0 OFFSET(24) NUMBITS(2) [],
        /// Training-pattern 1 to 2 transition delay.
        TP1_TO_TP2_TIME OFFSET(20) NUMBITS(3) [],
        /// FDI delay setting.
        FDI_DELAY OFFSET(0) NUMBITS(8) []
    ],

    /// FDI RX transfer unit size register.
    pub FDI_RX_TUSIZE1 [
        /// Transfer unit size field.
        TU_SIZE OFFSET(25) NUMBITS(7) []
    ],

    /// FDI RX interrupt lock bits.
    pub FDI_RX_IIR [
        /// Inter-lane alignment lock status.
        INTERLANE_ALIGNMENT OFFSET(10) NUMBITS(1) [],
        /// Symbol lock status.
        SYMBOL_LOCK OFFSET(9) NUMBITS(1) [],
        /// Bit lock status.
        BIT_LOCK OFFSET(8) NUMBITS(1) []
    ],

    /// Ironlake CPU pipe configuration register.
    pub CPU_PIPECONF [
        ENABLE OFFSET(31) NUMBITS(1) [],
        /// Same non-numeric BPC encoding as `PIPECONF`.
        BPC OFFSET(5) NUMBITS(3) [
            Bits8 = 0,
            Bits10 = 1,
            Bits6 = 2,
            Bits12 = 3
        ],
        /// Pipe dither enable.
        DITHER OFFSET(4) NUMBITS(1) []
    ],

    /// Ironlake CPU primary plane control register.
    pub CPU_DSPCNTR [
        ENABLE OFFSET(31) NUMBITS(1) [],
        FORMAT OFFSET(26) NUMBITS(4) [
            Xrgb8888 = 6
        ],
        PIPE_SELECT OFFSET(24) NUMBITS(2) [
            PipeA = 0,
            PipeB = 1,
            PipeC = 2
        ]
    ],

    // ---------------------------------------------------------------------
    // Later-generation planning/stub metadata and AUX registers.
    // ---------------------------------------------------------------------

    /// Tigerlake DPLL selection/register hint value used by the current
    /// libgfxinit-compatible allocation stub.
    pub TGL_DPLL_SELECT [
        /// Raw PLL register hint. Current libgfxinit returns zero for invalid/no PLL.
        REGISTER_VALUE OFFSET(0) NUMBITS(32) []
    ],

    /// Tigerlake Type-C/DDI orientation metadata for future live routing.
    pub TGL_TYPEC_ORIENTATION [
        /// Encoded Type-C orientation. Current libgfxinit live path uses `None`.
        VALUE OFFSET(0) NUMBITS(2) [
            None = 0,
            Normal = 1,
            Reversed = 2
        ]
    ],

    /// Tigerlake hotplug detect status model for current no-op detection.
    pub TGL_HOTPLUG_STATUS [
        /// Any HPD detected bit. Current libgfxinit Tigerlake stub returns clear.
        DETECTED OFFSET(0) NUMBITS(1) []
    ],

    /// Intel DisplayPort AUX control register.
    pub DP_AUX_CTL [
        /// Start/request busy bit.
        SEND_BUSY OFFSET(31) NUMBITS(1) [],
        /// Transaction complete latch.
        DONE OFFSET(30) NUMBITS(1) [],
        /// Interrupt-on-done enable.
        INTERRUPT_ON_DONE OFFSET(29) NUMBITS(1) [],
        /// Timeout error latch.
        TIME_OUT_ERROR OFFSET(28) NUMBITS(1) [],
        /// Timeout timer selector.
        TIME_OUT_TIMER OFFSET(26) NUMBITS(2) [
            Timer400us = 0,
            Timer600us = 1,
            Timer800us = 2,
            Timer1600us = 3
        ],
        /// Receive error latch.
        RECEIVE_ERROR OFFSET(25) NUMBITS(1) [],
        /// Raw AUX message/response byte count.
        MESSAGE_SIZE OFFSET(20) NUMBITS(5) [],
        /// AUX precharge time field.
        PRECHARGE_TIME OFFSET(16) NUMBITS(4) [],
        /// AUX 2x bit-clock divider.
        BIT_CLOCK_2X_DIVIDER OFFSET(0) NUMBITS(11) []
    ]
];

register_bitfields! [u16,
    /// Legacy GMCH GCFGC PCI config register fields used for CDClk selection.
    pub GCFGC [
        /// GM965 CDClk selector. libgfxinit treats valid encodings 1..=3 as selector 0..=2.
        GM965_CDCLK_SELECT OFFSET(8) NUMBITS(5) [],
        /// GM45 CDClk selector.
        GM45_CDCLK_SELECT OFFSET(12) NUMBITS(1) [],
        /// G45 CDClk selector.
        G45_CDCLK_SELECT OFFSET(4) NUMBITS(3) []
    ]
];

register_structs! {
    /// Legacy GMCH display clock registers, relative to CLKCFG.
    pub GmchClockRegs {
        (0x00 => pub clkcfg: ReadWrite<u32, GMCH_CLKCFG::Register>),
        (0x04 => _reserved0),
        (0x38 => pub hpllvco: ReadWrite<u32, GMCH_HPLLVCO::Register>),
        (0x3c => @END),
    }
}

register_structs! {
    /// Legacy GTT fence register pair.
    pub LegacyFenceRegs {
        (0x00 => pub lower: ReadWrite<u32, FENCE_LOWER::Register>),
        (0x04 => pub upper: ReadWrite<u32, FENCE_UPPER::Register>),
        (0x08 => @END),
    }
}

register_structs! {
    /// Legacy graphics flush register block, relative to `GFX_FLSH_CNTL`.
    pub GfxFlushRegs {
        (0x00 => pub control: ReadWrite<u32, GFX_FLSH_CNTL::Register>),
        (0x04 => @END),
    }
}

impl GfxFlushRegs {
    /// Absolute offset of the legacy graphics/GTT flush control register.
    pub const OFFSET: usize = 0x02170;
}

register_structs! {
    /// Intel DisplayPort AUX channel register block, relative to AUX_CTL.
    pub DpAuxRegs {
        (0x00 => pub ctl: ReadWrite<u32, DP_AUX_CTL::Register>),
        (0x04 => pub data: [ReadWrite<u32>; 5]),
        (0x18 => _reserved0),
        (0x1c => pub mutex: ReadWrite<u32>),
        (0x20 => @END),
    }
}

register_structs! {
    /// Legacy GMCH hotplug register block, relative to `PORT_HOTPLUG_EN`.
    pub GmchHotplugRegs {
        (0x00 => pub enable: ReadWrite<u32, PORT_HOTPLUG_EN::Register>),
        (0x04 => pub status: ReadWrite<u32, PORT_HOTPLUG_STAT::Register>),
        (0x08 => @END),
    }
}

register_structs! {
    /// Legacy GMCH pipe timing register block, relative to pipe timing base.
    pub GmchPipeTimingRegs {
        (0x00 => pub htotal: ReadWrite<u32>),
        (0x04 => pub hblank: ReadWrite<u32>),
        (0x08 => pub hsync: ReadWrite<u32>),
        (0x0c => pub vtotal: ReadWrite<u32>),
        (0x10 => pub vblank: ReadWrite<u32>),
        (0x14 => pub vsync: ReadWrite<u32>),
        (0x18 => _reserved0),
        (0x1c => pub pipesrc: ReadWrite<u32>),
        (0x20 => @END),
    }
}

register_structs! {
    /// Split-PCH transcoder timing register block, relative to transcoder timing base.
    pub PchTranscoderTimingRegs {
        (0x00 => pub htotal: ReadWrite<u32>),
        (0x04 => pub hblank: ReadWrite<u32>),
        (0x08 => pub hsync: ReadWrite<u32>),
        (0x0c => pub vtotal: ReadWrite<u32>),
        (0x10 => pub vblank: ReadWrite<u32>),
        (0x14 => pub vsync: ReadWrite<u32>),
        (0x18 => @END),
    }
}

register_structs! {
    /// Legacy GMCH primary plane register block, relative to plane base.
    pub GmchPlaneRegs {
        (0x00 => pub cntr: ReadWrite<u32, DSPCNTR::Register>),
        (0x04 => pub addr: ReadWrite<u32>),
        (0x08 => pub stride: ReadWrite<u32>),
        (0x0c => pub pos: ReadWrite<u32>),
        (0x10 => pub size: ReadWrite<u32>),
        (0x14 => _reserved0),
        (0x1c => pub surf: ReadWrite<u32>),
        (0x20 => _reserved1),
        (0x24 => pub tileoff: ReadWrite<u32>),
        (0x28 => @END),
    }
}

register_structs! {
    /// Legacy GMCH panel power/fitter register block, relative to `PP_STATUS`.
    pub GmchPanelRegs {
        (0x00 => pub pp_status: ReadWrite<u32, PP_STATUS::Register>),
        (0x04 => pub pp_control: ReadWrite<u32, PP_CONTROL::Register>),
        (0x08 => pub pp_on_delays: ReadWrite<u32, PP_ON_DELAYS::Register>),
        (0x0c => pub pp_off_delays: ReadWrite<u32, PP_OFF_DELAYS::Register>),
        (0x10 => pub pp_divisor: ReadWrite<u32, PP_DIVISOR::Register>),
        (0x14 => _reserved0),
        (0x30 => pub pfit_control: ReadWrite<u32, PFIT_CONTROL::Register>),
        (0x34 => pub pfit_pgm_ratios: ReadWrite<u32, PFIT_PGM_RATIOS::Register>),
        (0x38 => @END),
    }
}

impl GmchPanelRegs {
    /// Base offset of the legacy GMCH panel power/fitter block.
    pub const BASE: usize = 0x61200;
    /// Absolute offset of `PP_CONTROL`.
    pub const PP_CONTROL_OFFSET: usize = Self::BASE + 0x04;
    /// Absolute offset of `PP_ON_DELAYS`.
    pub const PP_ON_DELAYS_OFFSET: usize = Self::BASE + 0x08;
    /// Absolute offset of `PP_OFF_DELAYS`.
    pub const PP_OFF_DELAYS_OFFSET: usize = Self::BASE + 0x0c;
    /// Absolute offset of `PP_DIVISOR`.
    pub const PP_DIVISOR_OFFSET: usize = Self::BASE + 0x10;
    /// Absolute offset of `PFIT_CONTROL`.
    pub const PFIT_CONTROL_OFFSET: usize = Self::BASE + 0x30;
}

register_structs! {
    /// Ironlake-family CPU FDI TX register block, relative to `FDI_TX_CTL_*`.
    pub IronlakeFdiTxRegs {
        (0x00 => pub ctl: ReadWrite<u32, FDI_TX_CTL::Register>),
        (0x04 => @END),
    }
}

/// Absolute CPU FDI TX register offsets for Ironlake-family ports.
pub struct IronlakeFdiTxOffsets;

impl IronlakeFdiTxOffsets {
    /// FDI TX control register for port A.
    pub const CTL_A: usize = 0x60100;
    /// FDI TX control register for port B.
    pub const CTL_B: usize = 0x61100;
    /// FDI TX control register for port C.
    pub const CTL_C: usize = 0x62100;
}

register_structs! {
    /// Ironlake-family PCH FDI RX register block, relative to `FDI_RX*_CTL`.
    pub IronlakeFdiRxRegs {
        (0x00 => pub ctl: ReadWrite<u32, FDI_RX_CTL::Register>),
        (0x04 => pub misc: ReadWrite<u32, FDI_RX_MISC::Register>),
        (0x08 => pub iir: ReadWrite<u32, FDI_RX_IIR::Register>),
        (0x0c => pub imr: ReadWrite<u32, FDI_RX_IIR::Register>),
        (0x10 => _reserved0),
        (0x24 => pub tusize: ReadWrite<u32, FDI_RX_TUSIZE1::Register>),
        (0x28 => @END),
    }
}

/// Absolute PCH FDI RX register offsets for one Ironlake-family FDI receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IronlakeFdiRxOffsets {
    /// FDI RX control register.
    pub ctl: usize,
    /// FDI RX misc register.
    pub misc: usize,
    /// FDI RX transfer-unit size register.
    pub tusize: usize,
    /// FDI RX interrupt-mask register.
    pub imr: usize,
    /// FDI RX interrupt-identity register.
    pub iir: usize,
}

impl IronlakeFdiRxOffsets {
    const PORT_STRIDE: usize = 0x1000;
    const CTL_OFFSET: usize = 0x0c;

    /// FDI RX offsets for receiver A.
    pub const A: Self = Self::for_port_index(0);
    /// FDI RX offsets for receiver B.
    pub const B: Self = Self::for_port_index(1);
    /// FDI RX offsets for receiver C.
    pub const C: Self = Self::for_port_index(2);

    const fn for_port_index(index: usize) -> Self {
        let ctl = 0xf0000 + index * Self::PORT_STRIDE + Self::CTL_OFFSET;
        Self {
            ctl,
            misc: ctl + 0x04,
            iir: ctl + 0x08,
            imr: ctl + 0x0c,
            tusize: ctl + 0x24,
        }
    }
}

/// Legacy VGA control register absolute offset.
pub const GMCH_VGACNTRL_OFFSET: usize = 0x71400;
