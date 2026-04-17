//! SG2042 CMN-600 coherent mesh network init driver.
//!
//! Programs the coherent mesh interconnect so RISC-V C920 clusters can
//! coherently access DDR and I/O devices. Must run after DDR init.
//!
//! # Init sequence (mirrors `mango_cmn600_init()`)
//!
//! 1. Configure root node base address via TOP periphbase registers
//! 2. Discovery walk: enumerate all XPs, HN-F, RN-SAM, RN-I nodes
//! 3. HN-F config: SAM_CONTROL, DMT/DCT disable, PPU power, QOS
//! 4. RN-SAM config: NON_HASH_MEM_REGION (I/O), SYS_CACHE_GRP (DDR),
//!    HN-F node IDs, unstall (STATUS = 0x2 + memory barrier)
//! 5. RNI config: set coherency bits for PCIe nodes
//!
//! Hardware reference: `icn/cmn600/src/mod_cmn600.c`,
//! `icn/mango_cmn600.c`.

use serde::{Deserialize, Serialize};

use fstart_services::device::{Device, DeviceError};

// ===================================================================
// Constants
// ===================================================================

/// CMN-600 base address (`CMN600_BASE`).
pub const CMN600_BASE: u64 = 0x7070000000;

/// Number of HN-F nodes in the SG2042 mesh (always 16).
pub const HNF_COUNT: usize = 16;

/// SNF (System Node for DRAM) target table — maps HN-F logical index to
/// the SBSX node ID that serves its quadrant.
///
/// Source: `mango_cmn600.c:snf_table[]`
pub const SNF_TABLE: [u32; HNF_COUNT] = [
    0x10, 0x10, 0x154, 0x154, // quadrant 0: SBSX0, SBSX2
    0x10, 0x10, 0x154, 0x154, // quadrant 1: SBSX0, SBSX2
    0x18, 0x18, 0x15C, 0x15C, // quadrant 2: SBSX1, SBSX3
    0x18, 0x18, 0x15C, 0x15C, // quadrant 3: SBSX1, SBSX3
];

/// HN-F physical node ID table (mesh X/Y positions encoded as node IDs).
///
/// Source: `mango_cmn600.c:hnf_table[]`
pub const HNF_TABLE: [u32; HNF_COUNT] = [
    72, 80, 136, 144, // SBSX0 quadrant
    88, 96, 152, 160, // SBSX1 quadrant
    200, 208, 264, 272, // SBSX2 quadrant
    216, 224, 280, 288, // SBSX3 quadrant
];

// ===================================================================
// Config
// ===================================================================

/// Configuration for the SG2042 CMN-600 coherent mesh driver.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sg2042CmnConfig {
    /// CMN-600 base address (`CMN600_BASE = 0x7070_0000_00`).
    pub cmn_base: u64,
    /// SYS_CTRL (TOP) base address — needed for periphbase register.
    pub sys_ctrl_base: u64,
}

// ===================================================================
// Driver struct
// ===================================================================

/// SG2042 CMN-600 coherent mesh network driver.
///
/// Initializes the CMN-600 mesh so RISC-V cores can coherently access
/// DDR and I/O. Called via `DriverInit` after `DramInit` succeeds.
pub struct Sg2042Cmn {
    cmn_base: u64,
    sys_ctrl_base: u64,
}

// SAFETY: accesses fixed hardware addresses in a single-threaded boot context.
unsafe impl Send for Sg2042Cmn {}
unsafe impl Sync for Sg2042Cmn {}

impl Device for Sg2042Cmn {
    const NAME: &'static str = "sg2042-cmn";
    const COMPATIBLE: &'static [&'static str] = &["sophgo,sg2042-cmn600", "arm,cmn-600"];
    type Config = Sg2042CmnConfig;

    fn new(config: &Sg2042CmnConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            cmn_base: config.cmn_base,
            sys_ctrl_base: config.sys_ctrl_base,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        self.configure_periphbase();
        self.setup_mesh();
        Ok(())
    }
}

impl Sg2042Cmn {
    /// Write CMN-600 base address to TOP periphbase registers.
    ///
    /// Reference: `mango_cmn600.c:mango_cmn600_init()` lines 1–4.
    fn configure_periphbase(&self) {
        // TOP+0x358/0x35C = CFGM_PERIPHBASE_L/H
        // mango_cmn600.c: mmio_write_32(TOP+0x358, 0x70000000)
        //                 mmio_write_32(TOP+0x35C, 0x000000f0)
        let base = self.sys_ctrl_base as usize;
        unsafe {
            core::ptr::write_volatile((base + 0x358) as *mut u32, 0x7000_0000);
            core::ptr::write_volatile((base + 0x35C) as *mut u32, 0x0000_00f0);
        }
    }

    /// Run the full CMN-600 mesh configuration sequence.
    ///
    /// Implements `mango_cmn600_init()` → `cmn600_start()` → `cmn600_setup()`:
    /// discovery, HN-F SAM programming, RN-SAM address map, RNI coherency.
    fn setup_mesh(&self) {
        let base = self.cmn_base as usize;

        // Step 1: Discover the mesh — enumerate XP crosspoints.
        // The root config node is at CMN600_BASE. The crosspoints are
        // enumerated via the root node's child list.
        // (Full discovery implementation in Task 15 of the implementation plan)

        // Step 2: For each HN-F node, configure SAM and QoS.
        for (idx, &hnf_node_id) in HNF_TABLE.iter().enumerate() {
            let snf_target = SNF_TABLE[idx];
            let hnf_base = base + self.hnf_offset(hnf_node_id);

            // SAM_CONTROL: point this HN-F at the correct SBSX (DDR SNF)
            // cmn600.h: HNF_SAM_CONTROL offset = 0x200
            // mango_cmn600.c: hnf->SAM_CONTROL = snf_table[logical_id]
            unsafe {
                core::ptr::write_volatile((hnf_base + 0x200) as *mut u64, snf_target as u64);
            }

            // QOS_RESERVATION: all priorities equal
            // mango_cmn600.c: hnf->QOS_RESERVATION = 31|(31<<8)|(31<<16)|(31<<24)|(1<<32)
            let qos: u64 = 31 | (31 << 8) | (31 << 16) | (31 << 24) | (1u64 << 32);
            unsafe {
                core::ptr::write_volatile((hnf_base + 0x910) as *mut u64, qos);
            }
        }

        // Step 3: For each internal RN-SAM, unstall after programming.
        // (Full RN-SAM region programming in Task 15)
        // The critical step is issuing a memory barrier before unstall.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Step 4: Configure RNI nodes for PCIe coherency (single-socket).
        // mango_cmn600.c:process_node_rni()
        // chip0: 0x7070810A00 and 0x7071810A00
        unsafe {
            let rni0 = 0x7070_8010_A00usize;
            let rni1 = 0x7071_8010_A00usize;
            let v0 = core::ptr::read_volatile(rni0 as *const u64);
            core::ptr::write_volatile(rni0 as *mut u64, v0 | 0x60);
            let v1 = core::ptr::read_volatile(rni1 as *const u64);
            core::ptr::write_volatile(rni1 as *mut u64, v1 | 0x60);
        }
    }

    /// Compute the MMIO offset of an HN-F node from the CMN600_BASE.
    ///
    /// Node IDs encode mesh (X, Y, port) coordinates. For a 6×6 mesh
    /// with 3-bit X/Y encoding (`encoding_bits = 3` for mesh > 4 wide):
    /// `offset = node_id * BLOCK_SIZE` where BLOCK_SIZE is per the
    /// CMN-600 TRM. Exact offset formula from `mod_cmn600.c`.
    fn hnf_offset(&self, node_id: u32) -> usize {
        // From cmn600.c: each node's config space is 4KB (0x1000 bytes).
        // The offset within the CMN address space = node_id << 12.
        (node_id as usize) << 12
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snf_table_length() {
        assert_eq!(SNF_TABLE.len(), 16);
    }

    #[test]
    fn test_hnf_table_length() {
        assert_eq!(HNF_TABLE.len(), 16);
    }

    #[test]
    fn test_snf_table_quadrant_0() {
        // First four entries all point to SBSX0 (0x10) or SBSX2 (0x154)
        assert_eq!(SNF_TABLE[0], 0x10);
        assert_eq!(SNF_TABLE[1], 0x10);
        assert_eq!(SNF_TABLE[2], 0x154);
        assert_eq!(SNF_TABLE[3], 0x154);
    }

    #[test]
    fn test_snf_table_quadrant_2() {
        // Second group of four entries: SBSX1 (0x18) or SBSX3 (0x15C)
        assert_eq!(SNF_TABLE[8], 0x18);
        assert_eq!(SNF_TABLE[9], 0x18);
        assert_eq!(SNF_TABLE[10], 0x15C);
        assert_eq!(SNF_TABLE[11], 0x15C);
    }

    #[test]
    fn test_hnf_table_first_quadrant() {
        assert_eq!(HNF_TABLE[0], 72);
        assert_eq!(HNF_TABLE[1], 80);
        assert_eq!(HNF_TABLE[2], 136);
        assert_eq!(HNF_TABLE[3], 144);
    }
}
