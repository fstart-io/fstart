//! Pineview DDR2 raminit — ported from coreboot `raminit.c`.
//!
//! This module implements the complete DDR2 SDRAM initialization sequence
//! for the Intel Atom D4xx/D5xx (Pineview) memory controller. The code
//! is a line-by-line port of coreboot's ~2600-line `raminit.c`, adapted
//! to Rust idioms and the fstart register access model.
//!
//! ## Boot paths
//!
//! - **Normal**: full SPD read → timing selection → PHY training.
//! - **Reset**: skip DLL timing and RCOMP (already calibrated).
//! - **Resume (S3)**: skip JEDEC init and some calibration.
//!
//! ## Entry point
//!
//! [`sdram_initialize`] is called from `IntelPineview::init()` with the
//! MCHBAR accessor, ECAM handle, and SPD addresses.

mod jedec;
mod mmap;
mod phy;
mod rcomplut;
mod spd;
mod timing;

use crate::MmioBar;
use crate::generic::spd::{DimmInfo, MemClock};
use crate::pineview::regs::{MchBar, mchbar};
use fstart_core::services::ServiceError;
use fstart_pci::ecam;

// ===================================================================
// Constants
// ===================================================================

pub const TOTAL_CHANNELS: usize = 1;
pub const TOTAL_DIMMS: usize = 2;
pub const RANKS_PER_CHANNEL: usize = 4;

pub const DIMM_TYPE_NONE: u8 = 0;
pub const DIMM_TYPE_UBDIMM: u8 = 1;
pub const DIMM_TYPE_SODIMM: u8 = 2;

pub const PLATFORM_DESKTOP: u8 = 0;
pub const PLATFORM_MOBILE: u8 = 1;

// ===================================================================
// Sysinfo — raminit state
// ===================================================================

/// Pineview FSB clocks; discriminants retain the hardware-table encoding.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FsbClock {
    #[default]
    Fsb667 = 0,
    Fsb800 = 1,
}

/// Selected memory timings (in clock cycles).
#[derive(Debug, Default, Clone, Copy)]
pub struct Timings {
    pub cas: u8,
    pub fsb_clock: FsbClock,
    pub mem_clock: MemClock,
    pub tras: u8,
    pub trp: u8,
    pub trcd: u8,
    pub twr: u8,
    pub trfc: u8,
    pub twtr: u8,
    pub trrd: u8,
    pub trtp: u8,
}

/// PLL parameters for DQS/DQ calibration.
#[derive(Debug, Clone)]
pub struct PllParam {
    pub kcoarse: [[u8; 72]; 2],
    pub pi: [[u8; 72]; 2],
    pub dben: [[u8; 72]; 2],
    pub dbsel: [[u8; 72]; 2],
    pub clkdelay: [[u8; 72]; 2],
}

impl Default for PllParam {
    fn default() -> Self {
        Self {
            kcoarse: [[0; 72]; 2],
            pi: [[0; 72]; 2],
            dben: [[0; 72]; 2],
            dbsel: [[0; 72]; 2],
            clkdelay: [[0; 72]; 2],
        }
    }
}

/// Complete raminit state, analogous to coreboot's `struct sysinfo`.
#[derive(Debug)]
pub struct SysInfo {
    pub boot_path: crate::BootPath,
    pub platform_type: u8,
    pub dimm_type: u8,
    pub spd_map: [u8; 4],
    pub dimms: [Option<DimmInfo>; TOTAL_DIMMS * TOTAL_CHANNELS],
    pub dimm_config: [u8; TOTAL_CHANNELS],
    pub spd_type: u8,
    pub selected_timings: Timings,
    pub channel_capacity: [u32; TOTAL_CHANNELS],

    // DLL / calibration state
    pub maxpi: u8,
    pub pioffset: u8,
    pub pi: [u8; 8],
    pub coarsectrl: u16,
    pub coarsedelay: u16,
    pub mediumphase: u16,
    pub readptrdelay: u16,
    pub nodll: u8,
    pub r#async: u8,
    pub dt0mode: u8,
    pub ggc: u16,
    pub vref_value: u8,
}

#[derive(zerocopy::FromBytes, zerocopy::IntoBytes, zerocopy::Immutable, zerocopy::KnownLayout)]
#[repr(C)]
struct TrainingWire {
    magic: [u8; 8],
    spd: [[u8; 128]; 2],
    timings: [u8; 11],
    platform_type: u8,
    spd_map: [u8; 4],
    pi: [u8; 8],
    coarsectrl: zerocopy::byteorder::U16<zerocopy::byteorder::LittleEndian>,
    coarsedelay: zerocopy::byteorder::U16<zerocopy::byteorder::LittleEndian>,
    mediumphase: zerocopy::byteorder::U16<zerocopy::byteorder::LittleEndian>,
    readptrdelay: zerocopy::byteorder::U16<zerocopy::byteorder::LittleEndian>,
    vref: u8,
    reserved: u8,
}
fn timing_bytes(t: Timings) -> [u8; 11] {
    [
        t.cas,
        t.fsb_clock as u8,
        t.mem_clock as u8,
        t.tras,
        t.trp,
        t.trcd,
        t.twr,
        t.trfc,
        t.twtr,
        t.trrd,
        t.trtp,
    ]
}

impl SysInfo {
    fn raw_spd(&self) -> [[u8; 128]; 2] {
        core::array::from_fn(|slot| {
            let mut bytes = [0; 128];
            if let Some(dimm) = &self.dimms[slot] {
                bytes.copy_from_slice(&dimm.spd_data[..128]);
            }
            bytes
        })
    }
    fn restore_training(&mut self, bytes: &[u8]) -> bool {
        use zerocopy::FromBytes;
        let Ok(wire) = TrainingWire::ref_from_bytes(bytes) else {
            return false;
        };
        if wire.magic != *b"PVIEW001"
            || wire.reserved != 0
            || wire.spd != self.raw_spd()
            || wire.timings != timing_bytes(self.selected_timings)
            || wire.platform_type != self.platform_type
            || wire.spd_map != self.spd_map
            || wire.coarsectrl.get() > 15
            || wire.vref > 0x3f
            || wire
                .pi
                .iter()
                .any(|&pi| pi > self.maxpi || (u16::from(pi) << self.pioffset) > 0x3f)
        {
            return false;
        }
        self.pi = wire.pi;
        self.coarsectrl = wire.coarsectrl.get();
        self.coarsedelay = wire.coarsedelay.get();
        self.mediumphase = wire.mediumphase.get();
        self.readptrdelay = wire.readptrdelay.get();
        self.vref_value = wire.vref;
        true
    }
    fn capture_training(&self, output: &mut [u8]) -> Result<usize, ServiceError> {
        use zerocopy::IntoBytes;
        let wire = TrainingWire {
            magic: *b"PVIEW001",
            spd: self.raw_spd(),
            timings: timing_bytes(self.selected_timings),
            platform_type: self.platform_type,
            spd_map: self.spd_map,
            pi: self.pi,
            coarsectrl: self.coarsectrl.into(),
            coarsedelay: self.coarsedelay.into(),
            mediumphase: self.mediumphase.into(),
            readptrdelay: self.readptrdelay.into(),
            vref: self.vref_value,
            reserved: 0,
        };
        let bytes = wire.as_bytes();
        output
            .get_mut(..bytes.len())
            .ok_or(ServiceError::InvalidParam)?
            .copy_from_slice(bytes);
        Ok(bytes.len())
    }
    pub fn new(boot_path: crate::BootPath, platform_type: u8, spd_map: [u8; 4]) -> Self {
        Self {
            boot_path,
            platform_type,
            dimm_type: DIMM_TYPE_NONE,
            spd_map,
            dimms: [None, None],
            dimm_config: [0],
            spd_type: 0,
            selected_timings: Timings::default(),
            channel_capacity: [0],
            maxpi: 0,
            pioffset: 0,
            pi: [0; 8],
            coarsectrl: 0,
            coarsedelay: 0,
            mediumphase: 0,
            readptrdelay: 0,
            nodll: 0,
            r#async: 0,
            dt0mode: 0,
            ggc: 0,
            vref_value: 0,
        }
    }

    /// Check if a DIMM slot is populated.
    pub fn dimm_populated(&self, idx: usize) -> bool {
        self.dimms[idx].as_ref().is_some_and(|d| d.card_type != 0)
    }

    pub fn is_sodimm(&self) -> bool {
        self.dimm_type == DIMM_TYPE_SODIMM
    }

    /// SMBIOS inventory of every wired slot; Pineview addresses up to 4 GiB.
    fn memory_info(&self) -> fstart_core::memory_info::MemoryInfo {
        use fstart_core::memory_info::{MemoryDevice, MemoryInfo};
        let mts = match self.selected_timings.mem_clock {
            MemClock::Ddr667 => 667,
            MemClock::Ddr800 => 800,
        };
        let mut info = MemoryInfo::new(4096);
        for (slot, dimm) in self.dimms.iter().enumerate() {
            if self.spd_map[slot] == 0 {
                continue;
            }
            let slot = slot as u8;
            info.push(dimm.as_ref().map_or(MemoryDevice::empty(0, slot), |dimm| {
                crate::generic::spd::ddr2::memory_device(dimm, 0, slot, mts)
            }));
        }
        info
    }
}

/// Result of a successful DRAM initialization.
pub struct Initialized {
    pub total_bytes: u64,
    /// Length of the training record written to `capture`, if any.
    pub captured: Option<usize>,
    pub memory_info: fstart_core::memory_info::MemoryInfo,
}

// ===================================================================
// Top-level entry point
// ===================================================================

/// Initialize DDR2 SDRAM.
///
/// This is the main raminit entry point, equivalent to coreboot's
/// `sdram_initialize()`. Called from `IntelPineview::init()`.
///
/// # Arguments
/// * `mch` — MCHBAR MMIO accessor
/// * `smbus` — SMBus controller for SPD reads
/// * `boot_path` — selected cold, warm-reset, or S3-resume path
/// * `spd_addresses` — SMBus addresses of DIMM SPD EEPROMs (e.g., [0x50, 0x51, 0, 0])
pub fn sdram_initialize<B: fstart_core::services::SmBus + ?Sized>(
    mch: &MchBar,
    smbus: &mut B,
    boot_path: crate::BootPath,
    platform_type: u8,
    spd_addresses: &[u8; 4],
) -> Result<Initialized, ServiceError> {
    sdram_initialize_cached(
        mch,
        smbus,
        boot_path,
        platform_type,
        spd_addresses,
        None,
        &mut [],
    )
}

pub(super) fn sdram_initialize_cached<B: fstart_core::services::SmBus + ?Sized>(
    mch: &MchBar,
    smbus: &mut B,
    boot_path: crate::BootPath,
    platform_type: u8,
    spd_addresses: &[u8; 4],
    cached: Option<&[u8]>,
    capture: &mut [u8],
) -> Result<Initialized, ServiceError> {
    fstart_log::info!("raminit: starting DDR2 initialization");

    let mut si = SysInfo::new(boot_path, platform_type, *spd_addresses);

    // 1. Read SPD data from DIMMs.
    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_SPD);
    spd::read_spds(&mut si, smbus)?;

    // 2. Detect RAM speed (common frequency).
    timing::detect_ram_speed(&mut si, mch)?;

    // 3. Detect smallest common timings.
    timing::detect_smallest_params(&mut si)?;
    let replay = cached.is_some_and(|bytes| si.restore_training(bytes));
    if boot_path == crate::BootPath::S3Resume && !replay {
        fstart_log::error!("pineview: S3 requires matching SPD/timings and training cache");
        return Err(ServiceError::HardwareError);
    }
    fstart_log::info!(
        "pineview: training cache {}",
        if replay { "hit" } else { "miss" }
    );

    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_PHY);
    // 4. Enable HPET.
    // (Handled by platform code, not raminit.)

    // 5. Clock crossing.
    mch.setbits32(mchbar::CPCTL, 1 << 15);
    timing::clk_crossing(&si, mch);

    // 6. Check for reset.
    timing::check_reset(&si);

    // 7. Clock mode.
    timing::clkmode(&si, mch);

    // 8. Program timings.
    timing::sdram_timings(&si, mch);

    // 9. DLL timing (skip on reset path).
    if si.boot_path != crate::BootPath::WarmReset {
        phy::dll_timing(&mut si, mch);
    }

    // 10. RCOMP (skip on reset path).
    if si.boot_path != crate::BootPath::WarmReset {
        phy::rcomp(&si, mch)?;
    }

    // 11. ODT.
    phy::odt(&si, mch);

    // 12. Wait for RCOMP completion (skip on reset path).
    if si.boot_path != crate::BootPath::WarmReset {
        let mut timeout = 1_000_000u32;
        while (mch.read8(mchbar::COMPCTRL1) & 1) != 0 {
            timeout -= 1;
            if timeout == 0 {
                fstart_log::error!("raminit: RCOMP timeout");
                return Err(ServiceError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    // 13. Memory map.
    mmap::sdram_mmap(&si, mch);

    // 14. Enable DDR IO buffer.
    let iobuf = mch.read8(mchbar::C0IOBUFACTCTL);
    mch.write8(mchbar::C0IOBUFACTCTL, (iobuf & !0x3F) | 0x08);
    mch.setbits32(mchbar::C0RSTCTL, 1 << 0);

    // 15. RCOMP update (skip on reset path, matching coreboot: the update
    // refines the measurement step 10 skipped there).
    if si.boot_path != crate::BootPath::WarmReset {
        phy::rcomp_update(&si, mch);
    }

    mch.setbits32(mchbar::HIT4, 1 << 1);

    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_JEDEC);
    // 16. JEDEC init (skip on S3 resume).
    if si.boot_path != crate::BootPath::S3Resume {
        mch.setbits32(mchbar::C0CKECTRL, 1 << 27);
        jedec::jedec_init(&si, mch);
    }

    // 17. Misc.
    jedec::sdram_misc(&si, mch);

    // 18. ZQCL.
    jedec::sdram_zqcl(&si, mch);

    // 19. Refresh control (skip on resume).
    if si.boot_path != crate::BootPath::S3Resume {
        mch.setbits32(mchbar::C0REFRCTRL2, 3 << 30);
    }

    // 20. DRA/DRB.
    mmap::sdram_dradrb(&mut si, mch);

    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_TRAINING);
    // 21. Receive enable calibration.
    phy::sdram_rcven(&mut si, mch, replay)?;

    // Desktop UDIMMs use the vendor-derived Vref margining path. Pineview
    // coreboot has no equivalent pass; keep this separate from its SO-DIMM
    // fixed-Vref flow until the vendor provenance is documented.
    if !si.is_sodimm() {
        if !replay {
            phy::sdram_vref_margining(&mut si, mch)?;
        }
        phy::update_vref_value(si.vref_value, mch);
    }

    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_FINALIZE);
    // 22. New tRD.
    phy::sdram_new_trd(&si, mch);

    // 23. Memory map registers.
    mmap::sdram_mmap_regs(&si, mch);

    // 24. Enhanced mode.
    phy::sdram_enhanced_mode(&si, mch);

    // 25. Power settings.
    phy::sdram_power_settings(&si, mch);

    // 26. Program DDR.
    phy::sdram_program_ddr(mch);

    // 27. Program DQDQS.
    phy::sdram_program_dqdqs(&si, mch);

    // 28. Periodic RCOMP.
    phy::sdram_periodic_rcomp(&si, mch);

    // 29. Set init done.
    mch.setbits32(mchbar::C0REFRCTRL2, 1 << 30);

    // 30. Tell ICH7 and northbridge we're done.
    ecam::EcamDevice::new(0, 0x1f, 0).and8(0xA2, !(1 << 7));
    ecam::EcamDevice::new(0, 0, 0).or8(0xF4, 1);

    // Compute total DRAM size from channel capacity.
    let total_mb = si.channel_capacity.iter().sum::<u32>();
    let total_bytes = (total_mb as u64) * 1024 * 1024;

    fstart_log::info!(
        "raminit: DDR2 initialization complete, {} MiB detected",
        total_mb
    );
    let captured = if boot_path != crate::BootPath::S3Resume && !capture.is_empty() {
        Some(si.capture_training(capture)?)
    } else {
        None
    };
    Ok(Initialized {
        total_bytes,
        captured,
        memory_info: si.memory_info(),
    })
}
