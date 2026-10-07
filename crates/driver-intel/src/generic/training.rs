//! Pure hashing of driver-supplied memory-training hardware and policy facts.
//!
//! Chipset drivers own discovery. Platform flows own cache acceptance and the
//! decision to reset on resume failure; this module performs no hardware I/O.

use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub struct HardwareIdentity {
    pub vendor_id: u16,
    pub device_id: u16,
    pub revision_id: u8,
    pub capabilities: [u32; 2],
    pub cpu_signature: u32,
}

/// Identity excludes mutable boot-status registers. Full SPD/topology and
/// selected timings are validated inside each chipset's payload decoder.
pub fn identity(
    algorithm: &[u8],
    spd_map: &[u8; 4],
    ggc: u16,
    fsb_encoding: u8,
    hardware: HardwareIdentity,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(algorithm);
    hash.update(spd_map);
    hash.update(ggc.to_le_bytes());
    hash.update([fsb_encoding]);
    hash.update(hardware.vendor_id.to_le_bytes());
    hash.update(hardware.device_id.to_le_bytes());
    hash.update([hardware.revision_id]);
    for capability in hardware.capabilities {
        hash.update(capability.to_le_bytes());
    }
    hash.update(hardware.cpu_signature.to_le_bytes());
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_identity_tracks_hardware_and_board_policy() {
        let hw = HardwareIdentity {
            vendor_id: 0x8086,
            device_id: 0x2a00,
            revision_id: 1,
            capabilities: [2, 3],
            cpu_signature: 4,
        };
        let key = identity(b"gm965-v1", &[0x50, 0, 0x52, 0], 0x30, 2, hw);
        assert_eq!(key, identity(b"gm965-v1", &[0x50, 0, 0x52, 0], 0x30, 2, hw));
        assert_ne!(key, identity(b"gm965-v2", &[0x50, 0, 0x52, 0], 0x30, 2, hw));
        assert_ne!(key, identity(b"gm965-v1", &[0x50, 0, 0x53, 0], 0x30, 2, hw));
        assert_ne!(key, identity(b"gm965-v1", &[0x50, 0, 0x52, 0], 0x40, 2, hw));
        assert_ne!(key, identity(b"gm965-v1", &[0x50, 0, 0x52, 0], 0x30, 3, hw));
        assert_ne!(
            key,
            identity(
                b"gm965-v1",
                &[0x50, 0, 0x52, 0],
                0x30,
                2,
                HardwareIdentity {
                    capabilities: [2, 5],
                    ..hw
                }
            )
        );
    }
}
