//! Native Bay Trail DDR3 RAM init (coreboot `soc/intel/baytrail/romstage`
//! native series), for one channel with one memory-down module or DIMM.
//!
//! The data rate follows the SPD and the memory-rate fuse the vendor MRC
//! honors. The flow: SPD decode, D-unit timings, DDRIO PHY init (with the
//! DDR3 reset), pre-JEDEC setup and JEDEC init, then receive enable, write
//! leveling, read and write training, and finally the geometry, memory
//! map and lock. A short memory test guards the result.
//!
//! There is no S3 path: the reference trains on every boot.

pub mod cpgc;
pub mod ddrio;
pub mod ddrio_regs;
pub mod delay;
pub mod dunit;
pub mod mc;
pub mod training;

use crate::baytrail::iosf::Iosf;
use crate::baytrail::pmc;
use fstart_core::memory_info::MemoryInfo;
use fstart_core::services::ServiceError;
use fstart_driver_spd::ddr3::{self, Ddr3Attributes};
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::ReadWriteable;

pub use mc::{MemoryMap, Stolen};

/// Byte lanes of the 64-bit channel.
pub const STROBES: usize = 8;
/// Ranks the D-unit and DDRIO address per channel.
pub const NUM_RANKS: u32 = 2;

/// The D-unit's data-rate encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DdrFreq {
    Ddr800 = 0,
    Ddr1066 = 1,
    Ddr1333 = 2,
}

impl DdrFreq {
    /// Clock period in 1/256 ns.
    pub const fn tck(self) -> u32 {
        match self {
            DdrFreq::Ddr800 => 640,
            DdrFreq::Ddr1066 => 480,
            DdrFreq::Ddr1333 => 384,
        }
    }

    pub const fn mts(self) -> u16 {
        match self {
            DdrFreq::Ddr800 => 800,
            DdrFreq::Ddr1066 => 1066,
            DdrFreq::Ddr1333 => 1333,
        }
    }
}

/// The selected configuration and the trained delays.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub dimm: Ddr3Attributes,
    pub freq: DdrFreq,
    pub cas_latency: u32,
    pub write_latency: u32,
    pub ranks: u32,
    pub rcvn: [i32; STROBES],
    pub wdqs: [i32; STROBES],
    pub wdq: [i32; STROBES],
    pub rdq: [i32; STROBES],
}

/// What raminit leaves behind.
#[derive(Debug, Clone, Copy)]
pub struct Trained {
    pub map: MemoryMap,
    pub freq: DdrFreq,
    pub memory_info: MemoryInfo,
}

impl Params {
    fn new(spd: &[u8], fused: DdrFreq) -> Result<Self, ServiceError> {
        // Some SPDs ship with a bad CRC; their fields are still checked.
        let dimm = match ddr3::decode(spd) {
            Err(ddr3::Error::Crc) => {
                fstart_log::warn!("baytrail: SPD CRC mismatch, using the SPD anyway");
                ddr3::decode_without_crc(spd)
            }
            result => result,
        }
        .map_err(|error| {
            fstart_log::error!("baytrail: SPD decode failed: {}", error.name());
            ServiceError::HardwareError
        })?;
        // The PHY setup and training cover rank 0 of an unbuffered 64-bit
        // channel only; refuse anything else before touching the hardware.
        if dimm.ranks != 1 || dimm.bus_width != 64 || matches!(dimm.module_type, 1 | 5 | 8) {
            fstart_log::error!(
                "baytrail: unsupported DIMM: {} ranks, {}-bit bus, module type {}",
                dimm.ranks,
                dimm.bus_width,
                dimm.module_type
            );
            return Err(ServiceError::NotSupported);
        }
        let freq = mc::select_frequency(&dimm, fused);
        let cas_latency = mc::select_cas_latency(&dimm, freq.tck())?;
        let params = Self {
            dimm,
            freq,
            cas_latency,
            // DDR3 CWL is 5, 6 and 7 at 800, 1066 and 1333.
            write_latency: 5 + freq as u32,
            ranks: u32::from(dimm.ranks),
            rcvn: [0; STROBES],
            wdqs: [0; STROBES],
            wdq: [0; STROBES],
            rdq: [0; STROBES],
        };
        fstart_log::info!(
            "baytrail: DDR3-{} CL{} CWL{} {}R x{} {} MiB",
            freq.mts(),
            params.cas_latency,
            params.write_latency,
            dimm.ranks,
            dimm.width,
            dimm.size_mb
        );
        Ok(params)
    }
}

/// Address-as-data and walking-ones over 1 MiB in 4 KiB steps (coreboot
/// `memtest_region`); the number of mismatches, up to 16.
fn memtest_region(base: usize, size: usize) -> u32 {
    let mut errors = 0;
    let mut report = |addr: usize, wrote: u32, got: u32| {
        fstart_log::error!(
            "baytrail: memtest {:#010x}: wrote {:#010x} read {:#010x}",
            addr,
            wrote,
            got
        );
        errors += 1;
        errors >= 16
    };
    // SAFETY: trained DRAM below the carve-outs, not yet in use by anyone.
    unsafe {
        for addr in (base..base + size).step_by(4096) {
            let p = addr as *mut u32;
            p.write_volatile(addr as u32);
            p.add(1).write_volatile(!(addr as u32));
        }
        for addr in (base..base + size).step_by(4096) {
            let p = addr as *mut u32;
            let got = p.read_volatile();
            if got != addr as u32 && report(addr, addr as u32, got) {
                return 16;
            }
            let got = p.add(1).read_volatile();
            if got != !(addr as u32) && report(addr + 4, !(addr as u32), got) {
                return 16;
            }
        }
        for addr in (base..base + size).step_by(4096) {
            let p = addr as *mut u32;
            for bit in 0..32 {
                let pattern = 1u32 << bit;
                p.write_volatile(pattern);
                let got = p.read_volatile();
                if got != pattern && report(addr, pattern, got) {
                    return 16;
                }
            }
        }
    }
    errors
}

/// Train the channel from `spd` and carve the TXE UMA, graphics stolen
/// memory and TSEG below BMBOUND.
pub fn sdram_initialize<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    spd: &[u8],
    stolen: Stolen,
    txe_mb: u32,
) -> Result<Trained, ServiceError> {
    fstart_log::info!("baytrail: native raminit");
    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_SPD);
    let mut p = Params::new(spd, mc::fused_max_freq(iosf))?;
    mc::program_timings(iosf, &p);
    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_PHY);
    ddrio::init(iosf, &p)?;
    mc::pre_jedec(iosf, &p);
    ddrio::pre_jedec(iosf);
    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_JEDEC);
    mc::jedec_init(iosf, &p);

    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_TRAINING);
    training::receive_enable(iosf, &mut p)?;
    training::write_leveling(iosf, &mut p)?;
    training::read_training(iosf, &mut p)?;
    training::write_training(iosf, &mut p)?;

    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_FINALIZE);
    let map = mc::finalize(iosf, &p, stolen, txe_mb, crate::baytrail::TSEG_MB)?;
    // DRAM init done.
    pmc::regs().gen_pmcon1.modify(pmc::GEN_PMCON1::DISB::CLEAR);
    mc::lock(iosf);

    fstart_timestamp::add(fstart_timestamp::id::RAMINIT_MEMORY_TEST);
    let last = if map.tseg_base_mb > 1024 + 2 {
        0x4000_0000
    } else {
        ((map.tseg_base_mb - 2) as usize) << 20
    };
    let errors = memtest_region(0x10_0000, 0x10_0000)
        + memtest_region(0x100_0000, 0x10_0000)
        + memtest_region(last, 0x10_0000);
    if errors != 0 {
        fstart_log::error!("baytrail: memory test failed with {} errors", errors);
        return Err(ServiceError::HardwareError);
    }
    fstart_log::info!("baytrail: memory test passed");

    let mut memory_info = MemoryInfo::new(8192);
    let device = ddr3::inventory(spd, p.freq.mts(), 0, 0)
        .unwrap_or_else(|_| fstart_core::memory_info::MemoryDevice::unknown(0, 0));
    memory_info.push(device);
    Ok(Trained {
        map,
        freq: p.freq,
        memory_info,
    })
}

#[cfg(test)]
mod coreboot_parity;
