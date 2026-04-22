//! Intel High Definition Audio (HDA / Azalia) controller interface.
//!
//! The HDA specification (Intel Rev 1.0a, June 2010) defines a standard
//! MMIO register interface used by every HDA controller — Intel ICH/PCH,
//! AMD/ATI SB, Nvidia MCP, VIA VT8xxx, and others.  This crate provides:
//!
//! - **Controller operations**: reset, codec detection, verb programming
//!   via the Immediate Command interface (IC/IR/ICS registers).
//! - **Verb table types**: [`HdaVerbTable`] and [`HdaConfig`] for
//!   describing codec pin configurations in board RON files.
//! - **Helper functions**: [`hda_verb`], [`hda_pin_cfg`], [`hda_pin_nc`]
//!   matching coreboot's `AZALIA_VERB_12B`, `AZALIA_PIN_CFG`,
//!   `AZALIA_PIN_CFG_NC` macros.
//!
//! Chipset-specific PCI config quirks (ESD fixes, VC setup, clock gating)
//! stay in the individual chipset drivers — this crate only handles the
//! standardised HDA register interface that lives behind BAR0.
//!
//! # RON verb table format
//!
//! ```ron
//! hda: (
//!     verbs: [
//!         ( vendor_id: 0x10ec0662, subsystem_id: 0x105b0d55, verbs: [
//!             // AZALIA_PIN_CFG(0, 0x14, 0x01014c10) — rear line-out
//!             0x01471c10, 0x01471d4c, 0x01471e01, 0x01471f01,
//!             // ...
//!         ]),
//!     ],
//! )
//! ```
//!
//! Each 32-bit verb word encodes:
//! `[31:28]=codec_addr, [27:20]=NID, [19:8]=verb_id, [7:0]=payload`.

#![no_std]

use core::ptr;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// HDA standard register offsets (relative to BAR0)
// ---------------------------------------------------------------------------

/// Global Capabilities register (16-bit, RO / R/WO on some PCHs).
pub const REG_GCAP: usize = 0x00;
/// Global Control register (32-bit).
pub const REG_GCTL: usize = 0x08;
/// Controller Reset bit in GCTL.
pub const GCTL_CRST: u32 = 1 << 0;
/// STATESTS — codec status/change bits (16-bit at offset 0x0E).
pub const REG_STATESTS: usize = 0x0E;
/// Immediate Command register (32-bit).
pub const REG_IC: usize = 0x60;
/// Immediate Response register (32-bit, RO).
pub const REG_IR: usize = 0x64;
/// Immediate Command Status register (16-bit).
pub const REG_ICS: usize = 0x68;
/// ICS: Immediate Command Busy.
pub const ICS_BUSY: u16 = 1 << 0;
/// ICS: Immediate Result Valid.
pub const ICS_VALID: u16 = 1 << 1;

/// Maximum number of codec addresses (0..14, spec limit).
pub const MAX_CODECS: u8 = 15;

// ---------------------------------------------------------------------------
// Verb encoding helpers
// ---------------------------------------------------------------------------

/// Encode a single 12-bit HDA verb.
///
/// Equivalent to coreboot's `AZALIA_VERB_12B(codec, nid, verb_id, payload)`.
///
/// Format: `[31:28]=codec, [27:20]=nid, [19:8]=verb_id, [7:0]=payload`.
#[inline]
pub const fn hda_verb(codec: u32, nid: u32, verb_id: u32, payload: u32) -> u32 {
    (codec << 28) | (nid << 20) | (verb_id << 8) | payload
}

/// Expand a pin configuration value into four SET_CONFIGURATION_DEFAULT verbs.
///
/// Equivalent to coreboot's `AZALIA_PIN_CFG(codec, pin, val)`.
#[inline]
pub const fn hda_pin_cfg(codec: u32, pin: u32, val: u32) -> [u32; 4] {
    [
        hda_verb(codec, pin, 0x71c, (val >> 0) & 0xff),
        hda_verb(codec, pin, 0x71d, (val >> 8) & 0xff),
        hda_verb(codec, pin, 0x71e, (val >> 16) & 0xff),
        hda_verb(codec, pin, 0x71f, (val >> 24) & 0xff),
    ]
}

/// Pin configuration value for "not connected".
///
/// Equivalent to coreboot's `AZALIA_PIN_CFG_NC(seq)`.
#[inline]
pub const fn hda_pin_nc(seq: u32) -> u32 {
    0x411111f0 | (seq & 0xf)
}

/// Encode a SET_SUBSYSTEM_ID verb sequence (4 verbs writing to NID 1).
///
/// Equivalent to coreboot's `AZALIA_SUBVENDOR(codec, val)`.
#[inline]
pub const fn hda_subvendor(codec: u32, val: u32) -> [u32; 4] {
    [
        hda_verb(codec, 1, 0x720, (val >> 0) & 0xff),
        hda_verb(codec, 1, 0x721, (val >> 8) & 0xff),
        hda_verb(codec, 1, 0x722, (val >> 16) & 0xff),
        hda_verb(codec, 1, 0x723, (val >> 24) & 0xff),
    ]
}

/// GET_PARAMETER verb for reading a parameter from a widget.
///
/// Equivalent to coreboot's `AZALIA_VERB_GET_VENDOR_ID(codec)` when
/// called with `param = 0x00`.
#[inline]
pub const fn hda_get_param(codec: u32, nid: u32, param: u32) -> u32 {
    hda_verb(codec, nid, 0xF00, param)
}

// ---------------------------------------------------------------------------
// Verb table types (serde-compatible for board RON)
// ---------------------------------------------------------------------------

/// A single codec's verb table entry.
///
/// Each entry describes one HDA codec's pin configuration and custom
/// verb commands.  The verb table format follows coreboot's convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdaVerbTable {
    /// Codec vendor/device ID (e.g. `0x10ec0662` for Realtek ALC662).
    pub vendor_id: u32,
    /// Subsystem ID to write to the codec.
    pub subsystem_id: u32,
    /// Raw 32-bit HDA verbs to send to this codec.
    pub verbs: heapless::Vec<u32, 64>,
}

/// HD Audio configuration block for a board.
///
/// Contains verb tables for all codecs present on the board.
/// Placed in the southbridge/chipset driver config in the board RON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HdaConfig {
    /// Codec verb tables.  Up to 4 codecs on one HDA link.
    #[serde(default)]
    pub verbs: heapless::Vec<HdaVerbTable, 4>,
}

// ---------------------------------------------------------------------------
// HdaController — generic HDA MMIO interface
// ---------------------------------------------------------------------------

/// HDA controller register interface.
///
/// Operates on any HDA-compliant controller through BAR0 MMIO.
/// Chipset-specific PCI config programming (ESD, VC, clock detection)
/// is NOT included — that belongs in the chipset driver.
///
/// # Usage
///
/// ```ignore
/// // Chipset driver sets up PCI config, reads BAR0:
/// let bar0 = ecam::read32(0, hda_dev, hda_func, 0x10) & !0xF;
/// let hda = HdaController::new(bar0 as usize);
///
/// let codec_mask = hda.detect_codecs();
/// hda.program_verb_tables(&config.hda, codec_mask);
/// ```
pub struct HdaController {
    base: usize,
}

// SAFETY: MMIO-based, single-threaded firmware context.
unsafe impl Send for HdaController {}
unsafe impl Sync for HdaController {}

impl HdaController {
    /// Construct from the MMIO base address (BAR0 with low bits masked).
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    /// Return the MMIO base address.
    pub const fn base(&self) -> usize {
        self.base
    }

    // ---- Register access ----

    #[inline]
    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: base + offset is a valid HDA MMIO register.
        unsafe { ptr::read_volatile((self.base + offset) as *const u32) }
    }

    #[inline]
    fn write32(&self, offset: usize, val: u32) {
        // SAFETY: base + offset is a valid HDA MMIO register.
        unsafe { ptr::write_volatile((self.base + offset) as *mut u32, val) }
    }

    #[inline]
    fn read16(&self, offset: usize) -> u16 {
        // SAFETY: base + offset is a valid HDA MMIO register.
        unsafe { ptr::read_volatile((self.base + offset) as *const u16) }
    }

    #[inline]
    fn write16(&self, offset: usize, val: u16) {
        // SAFETY: base + offset is a valid HDA MMIO register.
        unsafe { ptr::write_volatile((self.base + offset) as *mut u16, val) }
    }

    // ---- Controller reset ----

    /// Enter reset (clear CRST, active-low).
    ///
    /// Returns `true` if the controller acknowledged the reset within
    /// the timeout.
    pub fn enter_reset(&self) -> bool {
        let gctl = self.read32(REG_GCTL);
        self.write32(REG_GCTL, gctl & !GCTL_CRST);

        // Wait for CRST to read back as 0 (up to 50 ms).
        for _ in 0..50_000 {
            if self.read32(REG_GCTL) & GCTL_CRST == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Exit reset (set CRST).
    ///
    /// Returns `true` if the controller is ready (CRST reads back as 1)
    /// within the timeout.
    pub fn exit_reset(&self) -> bool {
        let gctl = self.read32(REG_GCTL);
        self.write32(REG_GCTL, gctl | GCTL_CRST);

        for _ in 0..50_000 {
            if self.read32(REG_GCTL) & GCTL_CRST != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    // ---- Codec detection ----

    /// Detect codecs on the HDA link.
    ///
    /// Performs a full reset cycle (enter → exit → wait for codec init)
    /// and returns a bitmask of detected codec addresses (bits [14:0]).
    /// Returns 0 if no codecs are found.
    ///
    /// This implements the standard HDA codec discovery protocol
    /// (HDA Spec 1.0a §4.3 "Codec Discovery").
    pub fn detect_codecs(&self) -> u16 {
        // Lock GCAP (R/WO on some Intel PCHs, no-op on others).
        let gcap = self.read16(REG_GCAP);
        self.write16(REG_GCAP, gcap);

        // Clear any stale STATESTS bits.
        self.write16(REG_STATESTS, 0x7FFF);

        // Full reset cycle.
        if !self.enter_reset() {
            fstart_log::error!("hda: enter reset timeout");
            return 0;
        }
        if !self.exit_reset() {
            fstart_log::error!("hda: exit reset timeout");
            self.enter_reset();
            return 0;
        }

        // Codecs have up to 25 frames at 48kHz (≈521 µs) to signal.
        for _ in 0..600 {
            core::hint::spin_loop();
        }

        let mask = self.read16(REG_STATESTS) & 0x7FFF;
        if mask == 0 {
            self.enter_reset();
            fstart_log::info!("hda: no codecs detected");
        }
        mask
    }

    // ---- Immediate command interface ----

    /// Send a single verb via the Immediate Command interface and
    /// return the response.
    ///
    /// Uses the IC (Immediate Command), IR (Immediate Response), and
    /// ICS (Immediate Command Status) registers at offsets 0x60-0x68.
    ///
    /// Returns `None` on timeout.
    pub fn send_verb(&self, verb: u32) -> Option<u32> {
        // Wait for not busy.
        for _ in 0..10_000 {
            if self.read16(REG_ICS) & ICS_BUSY == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if self.read16(REG_ICS) & ICS_BUSY != 0 {
            return None;
        }

        // Write the verb to IC.
        self.write32(REG_IC, verb);
        // Set ICB to trigger the send.
        let ics = self.read16(REG_ICS);
        self.write16(REG_ICS, ics | ICS_BUSY);

        // Wait for response (IRV bit or busy clear).
        for _ in 0..10_000 {
            let status = self.read16(REG_ICS);
            if status & ICS_VALID != 0 {
                return Some(self.read32(REG_IR));
            }
            if status & ICS_BUSY == 0 {
                return Some(self.read32(REG_IR));
            }
            core::hint::spin_loop();
        }
        None
    }

    /// Read a codec's vendor/device ID.
    ///
    /// Sends GET_PARAMETER(VENDOR_ID) to NID 0 of the given codec address.
    pub fn read_codec_vendor_id(&self, codec_addr: u8) -> Option<u32> {
        self.send_verb(hda_get_param(codec_addr as u32, 0, 0x00))
    }

    // ---- Verb table programming ----

    /// Program verb tables for all detected codecs.
    ///
    /// For each codec address set in `codec_mask`, reads the codec's
    /// vendor/device ID and searches `config.verbs` for a matching
    /// [`HdaVerbTable`].  If found, sends all verbs in the table.
    ///
    /// Returns the number of codecs successfully programmed.
    pub fn program_verb_tables(&self, config: &HdaConfig, codec_mask: u16) -> u8 {
        let mut programmed = 0u8;

        for addr in 0u8..MAX_CODECS {
            if codec_mask & (1 << addr) == 0 {
                continue;
            }

            let viddid = match self.read_codec_vendor_id(addr) {
                Some(id) => id,
                None => {
                    fstart_log::error!("hda: codec {} vendor ID read timeout", addr);
                    continue;
                }
            };

            // Search verb tables for a matching vendor ID.
            let mut found = false;
            for table in &config.verbs {
                if table.vendor_id == viddid {
                    fstart_log::info!(
                        "hda: codec {} = {:#010x}, programming {} verbs",
                        addr,
                        viddid,
                        table.verbs.len() as u32,
                    );
                    for &verb in &table.verbs {
                        if self.send_verb(verb).is_none() {
                            fstart_log::error!("hda: verb {:#010x} timeout", verb);
                        }
                    }
                    programmed += 1;
                    found = true;
                    break;
                }
            }

            if !found {
                fstart_log::info!(
                    "hda: codec {} = {:#010x}, no verb table (using defaults)",
                    addr,
                    viddid,
                );
            }
        }

        programmed
    }
}
