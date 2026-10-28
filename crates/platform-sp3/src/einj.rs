//! Fixed EINJ communication ABI and firmware-owned injection targets.
//! The legacy low scratch page remains reserved. Additional socket-local
//! pages are selected from usable RAM and reserved before payload loading.
//! SMM follows no pointer from the shared communication page.

use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_driver_amd::sp3::{
    df::DramRule,
    psp::ras::{MAX_SOCKETS, MemoryTarget},
};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const BASE: u64 = crate::layout::STORE.end() - 0x1_0000;
pub const SCRATCH: u64 = BASE + 0x1000;
pub const SCRATCH_SIZE: u64 = 0x1000;
pub const SUPPORTED: u64 = 0;
pub const TRIGGER_POINTER: u64 = 8;
pub const BUSY: u64 = 16;
pub const STATUS: u64 = 24;
pub const LEGACY_TYPE: u64 = 32;
pub const PARAMETERS: u64 = 40;
pub const TRIGGER: u64 = 128;
pub const BACKEND_STATUS: u64 = 192;
/// Output only: suggested cache-line address per socket (zero = unavailable).
pub const TARGETS: u64 = 512;
pub const SOCKET_COUNT: u64 = TARGETS + 8 * MAX_SOCKETS as u64;
pub const SW_BEGIN: u8 = 0xd3;
pub const SW_END: u8 = 0xd4;
pub const SW_EXECUTE: u8 = 0xd5;
pub const MEMORY_CORRECTABLE: u32 = 8;

#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct Parameters {
    pub error_type: u32,
    pub vendor_offset: u32,
    pub flags: u32,
    pub apic_id: u32,
    pub address: u64,
    pub mask: u64,
    pub pcie_sbdf: u32,
}
impl Parameters {
    /// Only a cache line in a page owned by the installer. `owned` comes
    /// from immutable SMRAM configuration, never the shared TARGETS output.
    pub fn target(self, owned: &[u64]) -> Option<u64> {
        let granularity = !self.mask;
        let line = self.address & !63;
        (self.error_type == MEMORY_CORRECTABLE
            && self.vendor_offset == 0
            && self.flags == 2
            && granularity < SCRATCH_SIZE
            && granularity.wrapping_add(1).is_power_of_two()
            && owned
                .iter()
                .any(|&t| t != 0 && (t & !(SCRATCH_SIZE - 1)) == (line & !(SCRATCH_SIZE - 1))))
        .then_some(line)
    }
}

/// Find a disposable cache line for each socket without guessing NUMA
/// boundaries. Prefer the existing low page; otherwise intersect the live
/// DF rules with usable e820 RAM. Single-bit probes cover the supported
/// socket selector positions (bits 8..16), without scanning all DRAM.
/// Missing/disabled/unrecognized sockets remain unavailable (zero).
/// The caller must reserve each returned page before using the results.
pub fn plan_targets(
    sockets: u8,
    ram: &[E820Entry],
    rules: &[DramRule],
    locate: impl Fn(u64) -> Option<MemoryTarget>,
) -> [u64; MAX_SOCKETS] {
    if sockets == 0 || usize::from(sockets) > MAX_SOCKETS {
        return [0; MAX_SOCKETS];
    }
    core::array::from_fn(|socket| {
        if socket >= usize::from(sockets) {
            return 0;
        }
        let extra = ram
            .iter()
            .filter(|r| r.kind == E820Kind::Ram as u32)
            .flat_map(|r| {
                rules.iter().flat_map(move |rule| {
                    let start = r.addr.max(rule.base).saturating_add(SCRATCH_SIZE - 1)
                        & !(SCRATCH_SIZE - 1);
                    let end = r.addr.saturating_add(r.size).min(rule.end);
                    (0..10).filter_map(move |i| {
                        let address = start.checked_add(if i == 0 { 0 } else { 1 << (7 + i) })?;
                        let page = address & !(SCRATCH_SIZE - 1);
                        (page >= start && page.checked_add(SCRATCH_SIZE).is_some_and(|e| e <= end))
                            .then_some(address)
                    })
                })
            });
        (0..SCRATCH_SIZE)
            .step_by(64)
            .map(|o| SCRATCH + o)
            .chain(extra)
            .find(|&address| locate(address).is_some_and(|t| usize::from(t.socket) == socket))
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_owned_correctable_memory_is_accepted() {
        let owned = [SCRATCH, (1 << 37) + 64];
        let p = Parameters {
            error_type: 8,
            flags: 2,
            address: SCRATCH + 127,
            mask: !4095,
            ..Parameters::default()
        };
        assert_eq!(size_of::<Parameters>(), 36);
        assert_eq!(p.target(&owned), Some(SCRATCH + 64));
        assert_eq!(
            Parameters {
                address: (1 << 37) + 4095,
                ..p
            }
            .target(&owned),
            Some((1 << 37) + 4032)
        );
        for address in [
            SCRATCH - 1,
            SCRATCH + SCRATCH_SIZE,
            (1 << 37) + 4096,
            BASE,
            0,
        ] {
            assert_eq!(Parameters { address, ..p }.target(&owned), None);
        }
        for error_type in [0, 1, 0x10, 0x20, 0x80000008] {
            assert_eq!(Parameters { error_type, ..p }.target(&owned), None);
        }
        for mask in [0, !8191, !5] {
            assert_eq!(Parameters { mask, ..p }.target(&owned), None);
        }
        assert_eq!(Parameters { flags: 3, ..p }.target(&owned), None);
        assert_eq!(
            Parameters {
                vendor_offset: 8,
                ..p
            }
            .target(&owned),
            None
        );
        assert_eq!(p.target(&[0, 0]), None);
    }
    #[test]
    fn reserves_remote_ram_without_assuming_half_the_address_space() {
        let boundary = 1 << 37;
        let rules = [DramRule {
            base: 0,
            end: boundary * 2,
            dst: 0,
            hole: false,
            socket_interleave: false,
            die_interleave: 0,
            channel_interleave: 0,
        }];
        let ram = [E820Entry::new(boundary + 0x2000, 0x4000, E820Kind::Ram)];
        let locate = |address| {
            Some(MemoryTarget {
                socket: u8::from(address >= boundary),
                channel: 0,
                chip_select: 0,
            })
        };
        assert_eq!(
            plan_targets(2, &ram, &rules, locate),
            [SCRATCH, boundary + 0x2000]
        );
        assert_eq!(plan_targets(2, &[], &rules, locate), [SCRATCH, 0]);
        let interleaved = |address: u64| {
            Some(MemoryTarget {
                socket: ((address >> 11) & 1) as u8,
                channel: 0,
                chip_select: 0,
            })
        };
        assert_eq!(
            plan_targets(2, &[], &rules, interleaved),
            [SCRATCH, SCRATCH + 0x800]
        );
    }
}
