//! SG2042 PLIC interrupt routing driver.
//!
//! Programs A4SID (agent 4-bit secure ID) routing for all 16 RISC-V
//! clusters so the Platform-Level Interrupt Controller can route
//! interrupts to the correct cluster and hart.
//!
//! Only the single-socket Pioneer path is implemented. Multi-socket
//! CCIX paths are present as dead code gated by a config flag.
//!
//! Hardware reference: `icn/mango_plic.c` — `mango_plic_init()`.

use serde::{Deserialize, Serialize};

use fstart_services::device::{Device, DeviceError};

// ===================================================================
// Constants
// ===================================================================

/// PLIC base address (`PLIC_BASE`).
pub const PLIC_BASE: u64 = 0x7090000000;

/// Offset of the Sophgo custom config region within PLIC.
const PLIC_CFG_OFFSET: u64 = 0x1FE000;

/// PLIC config region base.
pub fn plic_cfg_base(plic_base: u64) -> u64 {
    plic_base + PLIC_CFG_OFFSET
}

// Config register offsets from `PLIC_CFG_BASE`
const PLIC_TARGET_ID_CTRL: u64 = 0x000;
#[allow(dead_code)] // single-socket Pioneer only uses cluster ctrl; kept for multi-socket future use
const PLIC_SOCKET0_CTRL: u64 = 0x004;
#[allow(dead_code)] // multi-socket path, not used on single-socket Pioneer
const PLIC_SOCKET1_CTRL: u64 = 0x008;
const PLIC_SOCKET0_CLUSTER_CTRL_BASE: u64 = 0x400;
#[allow(dead_code)] // multi-socket path
const PLIC_SOCKET1_CLUSTER_CTRL_BASE: u64 = 0x410;
#[allow(dead_code)] // multi-socket path
const PLIC_SOCKET0_A4SID_CTRL: u64 = 0x800;
#[allow(dead_code)] // multi-socket path
const PLIC_SOCKET1_A4SID_CTRL: u64 = 0x804;

// ===================================================================
// A4SID formula
// ===================================================================

/// Compute the A4SID for a cluster at mesh position (x, y).
///
/// The mesh cluster grid starts at (`MANGO_CLUSTER0_X=1`, `MANGO_CLUSTER0_Y=1`)
/// with a 4×4 layout. Cluster (1,1) is A4SID 0, cluster (4,4) is A4SID 15.
///
/// Formula: `(x - CLUSTER0_X) + (y - CLUSTER0_Y) * 4`
///        = `(x - 1) + (y - 1) * 4`
///
/// # Reference
///
/// `mango_misc.h`: MANGO_CLUSTER0_X=1, MANGO_CLUSTER0_Y=1.
pub fn a4sid(cluster_x: u8, cluster_y: u8) -> u8 {
    (cluster_x - 1) + (cluster_y - 1) * 4
}

/// Cluster mesh coordinates for SG2042 (16 clusters, 4×4 grid at X=1, Y=1).
///
/// Each entry is (X, Y). Clusters are laid out row-major starting from
/// (1,1). Source: `mango_misc.h:MANGO_CLUSTER0_X/Y`.
const CLUSTER_COORDS: [(u8, u8); 16] = [
    (1, 1),
    (2, 1),
    (3, 1),
    (4, 1), // row Y=1
    (1, 2),
    (2, 2),
    (3, 2),
    (4, 2), // row Y=2
    (1, 3),
    (2, 3),
    (3, 3),
    (4, 3), // row Y=3
    (1, 4),
    (2, 4),
    (3, 4),
    (4, 4), // row Y=4
];

// ===================================================================
// Config
// ===================================================================

/// Configuration for the SG2042 PLIC interrupt routing driver.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sg2042PlicConfig {
    /// PLIC base address (`PLIC_BASE = 0x7090_0000_00`).
    pub plic_base: u64,
    /// Number of RISC-V clusters to configure (16 on Pioneer).
    pub cluster_count: u8,
    /// Socket ID — must be 0 for Pioneer (single-socket).
    pub socket_id: u8,
}

// ===================================================================
// Driver struct
// ===================================================================

/// SG2042 PLIC A4SID routing driver.
pub struct Sg2042Plic {
    plic_base: u64,
    cluster_count: u8,
}

// SAFETY: accesses fixed hardware addresses; single-threaded boot context.
unsafe impl Send for Sg2042Plic {}
unsafe impl Sync for Sg2042Plic {}

impl Device for Sg2042Plic {
    const NAME: &'static str = "sg2042-plic";
    const COMPATIBLE: &'static [&'static str] = &["sophgo,sg2042-plic", "riscv,plic0"];
    type Config = Sg2042PlicConfig;

    fn new(config: &Sg2042PlicConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            plic_base: config.plic_base,
            cluster_count: config.cluster_count,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        let cfg_base = plic_cfg_base(self.plic_base);

        // Write own A4SID (=3) and socket ID (=0) to TARGET_ID_CTRL.
        // mango_plic.c: tmp = (socket_id << 0) | (3 << 8)
        let target_id = (0u32 << 0) | (3u32 << 8);
        self.plic_write(cfg_base + PLIC_TARGET_ID_CTRL, target_id);

        // Program cluster A4SIDs for socket 0.
        self.setup_cluster_a4sids(cfg_base + PLIC_SOCKET0_CLUSTER_CTRL_BASE);

        // Single-socket Pioneer: done.
        Ok(())
    }
}

impl Sg2042Plic {
    /// Program cluster A4SIDs into the PLIC cluster control registers.
    ///
    /// Each 32-bit register holds 4 A4SIDs (8 bits each, LSB-first).
    /// 16 clusters → 4 registers.
    ///
    /// Reference: `mango_plic.c:plic_setup_cluster_a4sid()`
    fn setup_cluster_a4sids(&self, ctrl_base: u64) {
        let clusters = self.cluster_count.min(16) as usize;
        let regs = (clusters + 3) / 4; // round up to 4-cluster groups

        for reg_idx in 0..regs {
            let mut word: u32 = 0;
            for slot in 0..4 {
                let cluster_idx = reg_idx * 4 + slot;
                if cluster_idx >= clusters {
                    break;
                }
                let (cx, cy) = CLUSTER_COORDS[cluster_idx];
                let id = a4sid(cx, cy) as u32;
                word |= id << (slot * 8);
            }
            self.plic_write(ctrl_base + (reg_idx as u64) * 4, word);
        }
    }

    /// Write a 32-bit value to a PLIC register.
    ///
    /// Reference: `mango_plic.c:plic_register_write()`
    fn plic_write(&self, addr: u64, val: u32) {
        unsafe {
            core::ptr::write_volatile(addr as *mut u32, val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a4sid_origin() {
        // Cluster at mesh (1,1) → A4SID 0 (first cluster).
        assert_eq!(a4sid(1, 1), 0);
    }

    #[test]
    fn test_a4sid_x_increment() {
        // Same row (Y=1), incrementing X.
        assert_eq!(a4sid(2, 1), 1);
        assert_eq!(a4sid(3, 1), 2);
        assert_eq!(a4sid(4, 1), 3);
    }

    #[test]
    fn test_a4sid_y_increment() {
        // Same column (X=1), incrementing Y by 1 adds 4 to A4SID.
        assert_eq!(a4sid(1, 2), 4);
        assert_eq!(a4sid(1, 3), 8);
        assert_eq!(a4sid(1, 4), 12);
    }

    #[test]
    fn test_a4sid_max_cluster() {
        // Last cluster (4,4): (4-1) + (4-1)*4 = 3 + 12 = 15.
        assert_eq!(a4sid(4, 4), 15);
    }

    #[test]
    fn test_cluster_coords_count() {
        assert_eq!(CLUSTER_COORDS.len(), 16);
    }

    #[test]
    fn test_cluster_coords_first_row() {
        assert_eq!(CLUSTER_COORDS[0], (1, 1));
        assert_eq!(CLUSTER_COORDS[1], (2, 1));
        assert_eq!(CLUSTER_COORDS[2], (3, 1));
        assert_eq!(CLUSTER_COORDS[3], (4, 1));
    }

    #[test]
    fn test_a4sid_all_16_unique() {
        let mut ids: [u8; 16] = [0; 16];
        for (i, &(x, y)) in CLUSTER_COORDS.iter().enumerate() {
            ids[i] = a4sid(x, y);
        }
        let mut sorted = ids;
        sorted.sort();
        // All 16 A4SIDs must be distinct
        for i in 1..16 {
            assert_ne!(sorted[i], sorted[i - 1], "duplicate a4sid at index {i}");
        }
    }
}
