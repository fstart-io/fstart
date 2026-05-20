//! Native Sandy Bridge DDR3 raminit scaffolding.
//!
//! This module intentionally follows coreboot's native raminit data flow rather
//! than the MRC wrapper path: read DDR3 SPD over the PCH I801 SMBus, derive DIMM
//! geometry/timing policy, then hand off to the controller/training sequence.
//! The full training sequence is still being ported; the code below provides the
//! native-only entry point and SPD/geometry state needed by the remaining port.

use fstart_arch_x86::udelay;
use fstart_pio::{pci_cfg_read32, pci_cfg_write32};
use fstart_services::ServiceError;
use fstart_smbus_intel::I801SmBus;
use fstart_spd::ddr3::{self, Ddr3DimmInfo};

mod command;
mod finalize;
mod iosav;
mod jedec;
mod mchbar;
mod memory_map;
mod mrs;
mod patterns;
mod phases;
mod read;
mod receive;
mod state;
mod timing;
mod training;
mod write;

const NUM_CHANNELS: usize = 2;
const NUM_SLOTS: usize = 2;
const HOST_DEV: u8 = 0;
const PCH_LPC_DEV: u8 = 0x1f;
const PCH_ME_DEV: u8 = 0x16;
const PCI_CPU_MEBASE_L: u8 = 0x70;
const PCI_CPU_MEBASE_H: u8 = 0x74;
const PCI_ME_HFS: u8 = 0x40;
const PCI_ME_UMA: u8 = 0x44;
const PCI_ME_H_GS: u8 = 0x4c;
const ME_RETRY: usize = 100_000;
const ME_DELAY_US: u32 = 10;
const ME_HFS_FPT_BAD: u32 = 1 << 5;
const ME_HFS_MODE_DEBUG: u8 = 2;
const ME_HFS_BIOS_DRAM_ACK: u8 = 1;
const ME_HFS_ACK_RESET: u8 = 1;
const ME_HFS_ACK_PWR_CYCLE: u8 = 2;
const ME_HFS_ACK_GBL_RESET: u8 = 6;
const ME_HFS_ACK_CONTINUE: u8 = 7;
const LPC_ETR3: u8 = 0xac;
const ETR3_CWORWRE: u32 = 1 << 18;
const ETR3_CF9GR: u32 = 1 << 20;
const ME_INIT_DONE: u32 = 1;
const ME_INIT_STATUS_SUCCESS: u32 = 0;

/// Decoded DDR3 SO-DIMM slot information.
#[derive(Clone, Copy, Default)]
pub struct DimmSlot {
    pub present: bool,
    pub spd_addr: u8,
    pub info: Option<Ddr3DimmInfo>,
}

impl DimmSlot {
    fn read(bus: &mut I801SmBus, addr: u8) -> Result<Self, ServiceError> {
        let mut spd = [0u8; 256];
        fstart_spd::read_spd(bus, addr, &mut spd)?;
        let info = ddr3::decode_dimm(&spd).ok_or(ServiceError::NotSupported)?;
        Ok(Self {
            present: true,
            spd_addr: addr,
            info: Some(info),
        })
    }

    fn capacity_bytes(&self) -> u64 {
        self.info
            .map(|info| u64::from(info.module_capacity_mb) << 20)
            .unwrap_or(0)
    }
}

/// Native raminit controller state, shaped after coreboot `ramctr_timing`.
#[derive(Clone, Copy)]
pub struct RaminitState {
    pub dimms: [[DimmSlot; NUM_SLOTS]; NUM_CHANNELS],
    pub total_capacity_bytes: u64,
    pub topology: state::ControllerTopology,
    pub timing: timing::TimingParams,
    pub mode_registers: [[mrs::ModeRegisters; NUM_SLOTS * 2]; NUM_CHANNELS],
    pub training: training::TrainingState,
    pub selected_mem_clock_mhz: u16,
    pub auto_self_refresh: bool,
}

impl Default for RaminitState {
    fn default() -> Self {
        Self {
            dimms: [[DimmSlot::default(); NUM_SLOTS]; NUM_CHANNELS],
            total_capacity_bytes: 0,
            topology: state::ControllerTopology::default(),
            timing: timing::TimingParams::default(),
            mode_registers: [[mrs::ModeRegisters::default(); NUM_SLOTS * 2]; NUM_CHANNELS],
            training: training::TrainingState::default(),
            selected_mem_clock_mhz: 0,
            auto_self_refresh: false,
        }
    }
}

/// Run the native-only Sandy Bridge DDR3 raminit path.
pub fn run_native_raminit(
    mchbar_base: u64,
    smbus_base: u16,
    spd_addresses: [u8; 4],
    max_mem_clock_mhz: u16,
    me_uma_size_mb: u32,
) -> Result<RaminitState, ServiceError> {
    let mut bus = I801SmBus::enable_on_i801(0, 31, 3, smbus_base);
    let mut state = RaminitState::default();

    for channel in 0..NUM_CHANNELS {
        for slot in 0..NUM_SLOTS {
            let idx = channel * NUM_SLOTS + slot;
            let addr = spd_addresses[idx];
            if addr == 0 {
                continue;
            }
            match DimmSlot::read(&mut bus, addr) {
                Ok(dimm) => {
                    state.total_capacity_bytes += dimm.capacity_bytes();
                    state.dimms[channel][slot] = dimm;
                }
                Err(ServiceError::NotSupported | ServiceError::HardwareError) => {
                    fstart_log::warn!("sandybridge: no valid DDR3 SPD at {:#x}", addr);
                }
                Err(err) => return Err(err),
            }
        }
    }

    if state.total_capacity_bytes == 0 {
        return Err(ServiceError::HardwareError);
    }

    state.auto_self_refresh = state
        .dimms
        .iter()
        .flat_map(|channel| channel.iter().filter_map(|dimm| dimm.info))
        .all(|info| info.supports_auto_self_refresh);
    state.topology = state::ControllerTopology::from_dimms(&state.dimms);
    state.timing = timing::select_timings(
        state
            .dimms
            .iter()
            .flat_map(|channel| channel.iter().filter_map(|dimm| dimm.info)),
        max_mem_clock_mhz,
    )?;
    state.selected_mem_clock_mhz = state.timing.mem_clock_mhz();
    state.training = training::TrainingState::for_topology(&state.topology);
    state.training.apply_timing(&state.timing);
    for channel in 0..NUM_CHANNELS {
        let regs = mrs::build_mode_registers(
            &state.timing,
            &state.topology,
            channel,
            state.auto_self_refresh,
        );
        for slotrank in 0..NUM_SLOTS * 2 {
            if (state.topology.rankmap[channel] & (1 << slotrank)) != 0 {
                state.mode_registers[channel][slotrank] = regs;
            }
        }
    }
    mchbar::program_pre_training(
        mchbar_base as usize,
        &state.topology,
        &state.timing,
        &state.training,
        me_uma_size_mb,
    )?;
    jedec::initialize_dram(mchbar_base as usize, &state)?;
    phases::prepare_training(mchbar_base as usize, &state)?;
    receive::calibrate(mchbar_base as usize, &mut state)?;
    read::train(mchbar_base as usize, &mut state)?;
    write::train(mchbar_base as usize, &mut state)?;
    command::train(mchbar_base as usize, &mut state)?;
    read::aggressive_train(mchbar_base as usize, &mut state)?;
    write::aggressive_train(mchbar_base as usize, &mut state)?;
    finalize::normalize_training(mchbar_base as usize, &mut state);
    finalize::set_read_write(mchbar_base as usize, &state);
    finalize::channel_test(mchbar_base as usize, &state)?;
    finalize::final_programming(mchbar_base as usize, &state);
    early_me_init_done()?;
    fstart_log::info!(
        "sandybridge: native DDR3 SPD parsed, total={} MiB, clock={} MHz, CL{}, rankmap={:02x}/{:02x}",
        state.total_capacity_bytes >> 20,
        state.selected_mem_clock_mhz,
        state.timing.cas,
        state.topology.rankmap[0],
        state.topology.rankmap[1]
    );

    // Native Sandy Bridge DDR3 raminit completed. Keep optional ECC scrub and
    // final lock-bit parity as follow-up work; X220 SO-DIMMs are non-ECC.
    Ok(state)
}

pub(crate) fn early_me_init_and_uma_size() -> Result<u32, ServiceError> {
    // Coreboot bd82x6x waits up to 1s for PCI_ME_UMA.valid before trusting
    // the requested ME UMA size, then rejects bad ME firmware.
    for _ in 0..ME_RETRY {
        let uma = me_cfg_read32(PCI_ME_UMA);
        if (uma & (1 << 16)) != 0 {
            let hfs = me_cfg_read32(PCI_ME_HFS);
            if (hfs & ME_HFS_FPT_BAD) != 0 {
                return Err(ServiceError::HardwareError);
            }
            return Ok(uma & 0x3f);
        }
        udelay(ME_DELAY_US);
    }
    Err(ServiceError::Timeout)
}

fn early_me_init_done() -> Result<(), ServiceError> {
    let hfs = me_cfg_read32(PCI_ME_HFS);
    let opmode = ((hfs >> 16) & 0x0f) as u8;
    let error_code = ((hfs >> 12) & 0x0f) as u8;
    if error_code != 0 {
        return Err(ServiceError::HardwareError);
    }

    // MEBASE from MESEG_BASE[35:20], same value coreboot passes in H_GS/DID.
    let mebase_l = host_cfg_read32(PCI_CPU_MEBASE_L);
    let mebase_h = host_cfg_read32(PCI_CPU_MEBASE_H) & 0x0f;
    let uma_base = (mebase_l >> 20) | (mebase_h << 12);
    let did = uma_base | (ME_INIT_STATUS_SUCCESS << 24) | (ME_INIT_DONE << 28);
    me_cfg_write32(PCI_ME_H_GS, did);

    if opmode == ME_HFS_MODE_DEBUG {
        return Ok(());
    }

    udelay(100);
    for _ in 0..=5000 {
        udelay(1000);
        let ack = ((me_cfg_read32(PCI_ME_HFS) >> 24) & 0xff) as u8;
        if ((ack & 0xf0) >> 4) == ME_HFS_BIOS_DRAM_ACK {
            let action = (ack & 0x0e) >> 1;
            return match action {
                0 | ME_HFS_ACK_CONTINUE => Ok(()),
                ME_HFS_ACK_RESET => perform_cf9_reset(false, 0x06),
                ME_HFS_ACK_PWR_CYCLE => perform_cf9_reset(false, 0x0e),
                ME_HFS_ACK_GBL_RESET => perform_cf9_reset(true, 0x0e),
                _ => Err(ServiceError::HardwareError),
            };
        }
    }
    Err(ServiceError::Timeout)
}

fn perform_cf9_reset(global: bool, reset_bits: u8) -> Result<(), ServiceError> {
    let mut etr3 = lpc_cfg_read32(LPC_ETR3);
    etr3 &= !ETR3_CWORWRE;
    if global {
        etr3 |= ETR3_CF9GR;
    } else {
        etr3 &= !ETR3_CF9GR;
    }
    lpc_cfg_write32(LPC_ETR3, etr3);
    // SAFETY: ME requested a chipset reset; writing CF9 is the hardware reset path.
    unsafe { fstart_pio::outb(0x0cf9, reset_bits) };
    loop {
        core::hint::spin_loop();
    }
}

fn host_cfg_read32(reg: u8) -> u32 {
    // SAFETY: Native raminit runs on the x86 BSP and reads host bridge config space.
    unsafe { pci_cfg_read32(0, HOST_DEV, 0, reg) }
}

fn lpc_cfg_read32(reg: u8) -> u32 {
    // SAFETY: Native raminit runs on the x86 BSP and reads LPC bridge config space.
    unsafe { pci_cfg_read32(0, PCH_LPC_DEV, 0, reg) }
}

fn lpc_cfg_write32(reg: u8, val: u32) {
    // SAFETY: Native raminit runs on the x86 BSP and writes LPC bridge config space.
    unsafe { pci_cfg_write32(0, PCH_LPC_DEV, 0, reg, val) }
}

fn me_cfg_read32(reg: u8) -> u32 {
    // SAFETY: Native raminit runs on the x86 BSP and reads PCH ME PCI config space.
    unsafe { pci_cfg_read32(0, PCH_ME_DEV, 0, reg) }
}

fn me_cfg_write32(reg: u8, val: u32) {
    // SAFETY: Native raminit runs on the x86 BSP and writes PCH ME PCI config space.
    unsafe { pci_cfg_write32(0, PCH_ME_DEV, 0, reg, val) }
}
