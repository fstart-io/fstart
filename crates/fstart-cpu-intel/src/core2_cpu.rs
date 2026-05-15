//! Intel Core/Core 2 CPU operations for GM965-era systems.
//!
//! Mirrors the per-CPU MSR setup in coreboot's `cpu/intel/model_6fx` driver
//! and optionally supplies an Intel microcode blob to `fstart-mp`.

use fstart_arch_x86::msr::{rdmsr, wrmsr};
use fstart_arch_x86::mtrr;
use fstart_mp::CpuOps;

const MSR_PKG_CST_CONFIG_CONTROL: u32 = 0xe2;
const MSR_PMG_IO_BASE_ADDR: u32 = 0xe4;
const MSR_PMG_IO_CAPTURE_ADDR: u32 = 0xe7;
const IA32_PECI_CTL: u32 = 0x5a0;
const IA32_PLATFORM_ID: u32 = 0x17;
const IA32_PERF_STATUS: u32 = 0x198;
const IA32_PERF_CTL: u32 = 0x199;
const IA32_MISC_ENABLE: u32 = 0x1a0;
const PIC_SENS_CFG: u32 = 0x1aa;
const HIGHEST_CLEVEL: u64 = 3;

fn configure_c_states(pmbase: u32) {
    // SAFETY: these MSRs are defined for Intel Core/Core 2 CPUs.
    unsafe {
        let mut msr = rdmsr(MSR_PKG_CST_CONFIG_CONTROL);
        msr |= 1 << 15; // config lock until next reset
        msr |= 1 << 14; // deeper sleep
        msr |= 1 << 10; // enable I/O MWAIT redirection for C-states
        msr &= !(1 << 9); // single stop grant disabled
        msr |= 1 << 3; // dynamic L2
        msr = (msr & !7) | HIGHEST_CLEVEL;
        wrmsr(MSR_PKG_CST_CONFIG_CONTROL, msr);

        let io_base = ((pmbase + 4) & 0xffff) as u64;
        wrmsr(MSR_PMG_IO_BASE_ADDR, io_base);

        let io_capture = ((pmbase + 4) as u64) | ((HIGHEST_CLEVEL - 2) << 16);
        wrmsr(MSR_PMG_IO_CAPTURE_ADDR, io_capture);
    }
}

fn configure_misc() {
    // SAFETY: these MSRs are defined for Intel Core/Core 2 CPUs.
    unsafe {
        let mut misc = rdmsr(IA32_MISC_ENABLE);
        misc |= 1 << 3; // TM1 enable
        misc |= 1 << 13; // TM2 enable
        misc |= 1 << 17; // Bidirectional PROCHOT#
        misc |= 1 << 10; // FERR# multiplexing
        misc |= 1 << 16; // Enhanced SpeedStep enable
        misc |= 1 << 26; // C2E
        misc |= 1 << 32; // C4E
        misc |= 1 << 33; // Hard C4E
        misc |= 1 << 36; // EMTTM
        wrmsr(IA32_MISC_ENABLE, misc);

        misc |= 1 << 20; // Lock Enhanced SpeedStep enable
        wrmsr(IA32_MISC_ENABLE, misc);

        let status = rdmsr(IA32_PERF_STATUS);
        let busratio_max = (status >> 40) & 0x1f;
        let platform = rdmsr(IA32_PLATFORM_ID);
        let vid_max = platform & 0x3f;
        let mut perf_ctl = status & !0xffff;
        perf_ctl |= busratio_max << 8;
        perf_ctl |= vid_max;
        wrmsr(IA32_PERF_CTL, perf_ctl);

        let mut peci = rdmsr(IA32_PECI_CTL);
        peci |= 1;
        wrmsr(IA32_PECI_CTL, peci);
    }
}

fn configure_pic_thermal_sensors() {
    // SAFETY: PIC_SENS_CFG is defined for this CPU family.
    unsafe {
        let mut msr = rdmsr(PIC_SENS_CFG);
        msr |= 1 << 21; // inter-core lock TM1
        msr |= 1 << 4; // enable bypass filter
        wrmsr(PIC_SENS_CFG, msr);
    }
}

/// CPU operations for Intel Core/Core 2 family 6 model f/16h systems.
pub struct Core2CpuOps {
    pmbase: u32,
    microcode: Option<&'static [u8]>,
}

impl Core2CpuOps {
    /// Create Core 2 CPU ops with the southbridge PMBASE and optional ucode.
    pub fn new(pmbase: u32, microcode: Option<&'static [u8]>) -> Self {
        Self { pmbase, microcode }
    }
}

impl CpuOps for Core2CpuOps {
    const NAME: &'static str = "Intel Core/Core 2";

    fn init_cpu(&self) {
        // SAFETY: MP init runs this on every active logical CPU. All CPUs
        // receive the same low-DRAM WB MTRR layout before OS handoff.
        unsafe { mtrr::setup_ram_wb() };
        configure_c_states(self.pmbase);
        configure_misc();
        configure_pic_thermal_sensors();
        fstart_log::info!("cpu: Core 2 MSR configuration complete");
    }

    fn microcode(&self) -> Option<(&[u8], bool)> {
        self.microcode.map(|blob| (blob, true))
    }
}
