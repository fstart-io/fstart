// SPDX-License-Identifier: MIT OR Apache-2.0
//! The CXL host bridges of an AMD IOD, which have no component registers
//! of their own: firmware builds each one's register block in DRAM (its
//! "shadow"), reserves an MMIO window for it ("trap") in the root bridge's
//! non-PCI MMIO and hands both to MPIO ([`super::mpio::trap_host_bridge`]),
//! which serves the OS's accesses to the window from the shadow. The HB
//! decoders arrive committed and locked (CxlV2Manager
//! `CxlPopulateComponentRegisters` / `CxlPopulateHDMDecoder`, Turin
//! CxlManager the same; reports/research/CXL.md §2.8).
//!
//! The decoders follow the vendor's `CxlPopulateHDMDecoder` `0x21300`
//! (CXL_COMPLETE.md §2.2.4) in what it sets: Target Type HDM-H for every
//! window, the device-coherent ones included, committed and locked. Two
//! things differ on purpose: the decoders are synthesized per host bridge
//! from the APOB windows, not per PCIe core through the vendor's
//! `CxlMgrGetApobInfo` (the same while a window's ports sit on one core;
//! SP7's root bridges have one CXL core each, the bonus core 6 has no
//! CXL); and the target list is in spec order (way 0 in bits 7:0). The
//! vendor shifts it MSB-first (`t = t << 8 | target[i]`), the reverse for
//! two or more ways, which Linux, matching downstream ports by position,
//! would misread; which order MPIO's trap or an OS expects is open (§4
//! question 3).
//!
//! The CEDT's windows follow the vendor's rules from the APOB windows
//! (`CxlMgrFillCfmWsInfoFromApob`, §2.9): one CFMWS per mapped window of
//! volatile, persistent or device-coherent memory, 256 MiB aligned, its
//! targets the host bridges of the APCB regions it interleaves.

use fstart_pci::cxl::{component, hdm};
use heapless::Vec;

use crate::apob::cxl::{MAX_TARGETS, MAX_WINDOWS, Memory, Window};

/// Bytes of a host bridge's emulated component register block.
pub const SHADOW_SIZE: usize = component::BLOCK_SIZE as usize;
/// The trapped MMIO window of a host bridge: 512 KiB, 256 KiB aligned,
/// above 4 GiB (`AllocateMmioForChbcr`; whether it must be above 4 GiB is
/// not known).
pub const TRAP_SIZE: u64 = 0x8_0000;
pub const TRAP_ALIGN: u64 = 0x4_0000;
/// What a host bridge's CHBS reports of the trap window.
pub const CHBS_LENGTH: u64 = component::BLOCK_SIZE;
/// The HDM decoder capability's offset from the CXL.cache/mem range.
const HDM_POINTER: u16 = 0x008;
/// Interleave targets an emulated host bridge decoder has.
pub const HDM_TARGETS: u8 = 4;
/// Most decoders of one emulated host bridge.
pub const MAX_DECODERS: usize = 8;

/// The root port an APCB region is behind: its host bridge's `_UID` and its
/// port number (PCIe Link Capabilities Port Number, the host bridge
/// decoder's target ID).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub uid: u32,
    pub port: u8,
}

/// One APOB window the CEDT publishes, with its targets resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowPlan {
    /// Index in the APOB's window list (QTG ID - 1).
    pub index: usize,
    pub base: u64,
    pub size: u64,
    pub socket: u8,
    pub memory: Memory,
    /// Host bridge interleave granularity in bytes.
    pub granularity: u32,
    /// The root ports of the interleaved regions, way 0 first.
    pub targets: Vec<Target, MAX_TARGETS>,
}

/// The windows of `windows` the CEDT describes; `target` resolves an APCB
/// region index to its root port. A window with a target that does not
/// resolve is left out (the vendor's `GetInterleaveTargetId` fails it).
pub fn plan(
    windows: &[Window],
    target: impl Fn(u8) -> Option<Target>,
) -> Vec<WindowPlan, MAX_WINDOWS> {
    let aligned = |v: u64| v.is_multiple_of(hdm::ALIGN);
    windows
        .iter()
        .enumerate()
        .filter(|(_, w)| {
            w.mapped()
                && matches!(
                    w.memory,
                    Memory::Volatile | Memory::Persistent | Memory::DeviceCoherent
                )
                && aligned(w.base)
                && aligned(w.size)
                && !w.targets.is_empty()
        })
        .filter_map(|(index, w)| {
            let targets = w
                .targets
                .iter()
                .map(|&i| target(i))
                .collect::<Option<Vec<Target, MAX_TARGETS>>>()?;
            Some(WindowPlan {
                index,
                base: w.base,
                size: w.size,
                socket: w.socket,
                memory: w.memory,
                granularity: granularity(w.interleave_size),
                targets,
            })
        })
        .collect()
}

/// The bytes of an APOB/APCB interleave size; no interleave (`0xFF`) and
/// unknown encodings are the smallest granularity. Assumes the IG encoding
/// (`256 << n`): the vendor's `CxlGetCfmwsHbig` was not decoded.
pub(crate) fn granularity(encoded: u8) -> u32 {
    hdm::granularity_bytes(encoded).unwrap_or(256)
}

impl WindowPlan {
    /// The window's host bridges in interleave order, consecutive repeats
    /// (several root ports of one host bridge) collapsed.
    pub fn host_bridges(&self) -> Vec<u32, MAX_TARGETS> {
        let mut out: Vec<u32, MAX_TARGETS> = Vec::new();
        for t in &self.targets {
            if out.last() != Some(&t.uid) {
                let _ = out.push(t.uid);
            }
        }
        out
    }

    /// The committed decoder host bridge `uid` routes the window with, to
    /// its root ports in the window: `None` if it is not a target.
    pub fn host_decoder(&self, uid: u32, lock: bool) -> Option<hdm::Decoder> {
        let ports: Vec<u8, 8> = self
            .targets
            .iter()
            .filter(|t| t.uid == uid)
            .map(|t| t.port)
            .take(8)
            .collect();
        if ports.is_empty() {
            return None;
        }
        // Inside a host bridge the root ports interleave at the window's
        // granularity times the host bridge ways (CXL 3.1 §9.13.1.1).
        let bridges = self.host_bridges().len() as u32;
        let granularity = if ports.len() > 1 {
            self.granularity * bridges
        } else {
            self.granularity
        };
        Some(hdm::Decoder {
            base: self.base,
            size: self.size,
            granularity,
            targets: ports,
            // The vendor's control is `IG | IW << 4 | 0x1100`: HDM-H for
            // every window, type 2's included.
            target_type: hdm::TargetType::HostOnly,
            lock_on_commit: lock,
            committed: true,
        })
    }

    /// The window's CFMWS: modulo arithmetic, restrictions by memory type
    /// (volatile 0x06, device-coherent 0x05, persistent 0x0A), QTG = index + 1.
    #[cfg(feature = "acpi")]
    pub fn cfmws(&self) -> fstart_acpi::cedt::Cfmws {
        use fstart_acpi::cedt::{Arithmetic, Cfmws, restrictions as r};
        Cfmws {
            base: self.base,
            size: self.size,
            targets: self.host_bridges().iter().copied().collect(),
            arithmetic: Arithmetic::Modulo,
            granularity: self.granularity,
            restrictions: match self.memory {
                Memory::DeviceCoherent => r::DEVICE_COHERENT | r::VOLATILE,
                Memory::Persistent | Memory::OtherPersistent => {
                    r::HOST_ONLY_COHERENT | r::PERSISTENT
                }
                _ => r::HOST_ONLY_COHERENT | r::VOLATILE,
            },
            qtg_id: self.index as u16 + 1,
        }
    }
}

/// The committed decoders of host bridge `uid` for `plans`, in address
/// order.
pub fn host_decoders(
    plans: &[WindowPlan],
    uid: u32,
    lock: bool,
) -> Vec<hdm::Decoder, MAX_DECODERS> {
    let mut decoders: Vec<hdm::Decoder, MAX_DECODERS> = plans
        .iter()
        .filter_map(|p| p.host_decoder(uid, lock))
        .take(MAX_DECODERS)
        .collect();
    decoders.sort_unstable_by_key(|d| d.base);
    decoders
}

/// Write the component registers of an emulated host bridge with
/// `decoders` into `shadow` (zeroed first): at 0x1000 the capability array
/// (`0x0111_0001`), the HDM decoder capability header (`0x0081_0005`), the
/// HDM decoder capability (count, 4 targets), global control (poison and
/// enable clear, as the vendor leaves them) and the decoders from 0x1018.
pub fn build(shadow: &mut [u8], decoders: &[hdm::Decoder]) -> Result<(), hdm::Error> {
    use hdm::Capability as Cap;
    use tock_registers::LocalRegisterCopy;
    if shadow.len() < SHADOW_SIZE || decoders.len() > MAX_DECODERS {
        return Err(hdm::Error::Ways(decoders.len()));
    }
    shadow[..SHADOW_SIZE].fill(0);
    let mut put = |offset: u32, value: u32| {
        let at = offset as usize;
        shadow[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    let cache_mem = component::CACHE_MEM;
    put(cache_mem, component::array_header(1));
    put(
        cache_mem + 4,
        component::capability_header(component::id::HDM_DECODER, 1, HDM_POINTER),
    );
    let hdm_base = cache_mem + u32::from(HDM_POINTER);
    let count = hdm::decoder_count(decoders.len() as u8).ok_or(hdm::Error::Ways(decoders.len()))?;
    let mut capability = LocalRegisterCopy::<u32, Cap::Register>::new(0);
    capability
        .write(Cap::DECODER_COUNT.val(count.into()) + Cap::TARGET_COUNT.val(HDM_TARGETS.into()));
    put(hdm_base + hdm::CAPABILITY, capability.get());
    put(hdm_base + hdm::GLOBAL_CONTROL, 0);
    for (n, decoder) in decoders.iter().enumerate() {
        let registers = decoder.registers()?;
        for (i, value) in registers.into_iter().enumerate() {
            put(hdm_base + hdm::decoder(n as u32) + 4 * i as u32, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apob::cxl::STATUS_OUT_OF_MEMORY_MAP;

    fn window(base: u64, size: u64, memory: Memory, targets: &[u8]) -> Window {
        Window {
            base,
            size,
            status: 0,
            socket: 0,
            nbio_map: 1,
            alignment: 28,
            interleave_size: 0xff,
            sub_interleave_map: [0; 4],
            sub_interleave_size: 0xff,
            memory,
            targets: Vec::from_slice(targets).unwrap(),
        }
    }

    /// Region 0 and 1 behind host bridge 2 (ports 0, 1), region 2 behind
    /// host bridge 5 (port 3).
    fn target(region: u8) -> Option<Target> {
        match region {
            0 => Some(Target { uid: 2, port: 0 }),
            1 => Some(Target { uid: 2, port: 1 }),
            2 => Some(Target { uid: 5, port: 3 }),
            _ => None,
        }
    }

    #[test]
    fn plans_follow_the_vendor_cfmws_rules() {
        const G: u64 = 1 << 30;
        let mut out_of_map = window(0x500 * G, 4 * G, Memory::Volatile, &[0]);
        out_of_map.status = STATUS_OUT_OF_MEMORY_MAP;
        let windows = [
            window(0x400 * G, 64 * G, Memory::Volatile, &[0, 1, 2]),
            out_of_map,
            window(0x600 * G + 0x1000, G, Memory::Volatile, &[2]),
            window(0x700 * G, G, Memory::OtherPersistent, &[2]),
            window(0x800 * G, G, Memory::Persistent, &[9]),
            window(0x900 * G, 2 * G, Memory::Persistent, &[2]),
        ];
        let plans = plan(&windows, target);
        assert_eq!(
            plans.iter().map(|p| p.index).collect::<std::vec::Vec<_>>(),
            [0, 5]
        );
        assert_eq!(plans[0].host_bridges()[..], [2, 5]);
        let hb2 = plans[0].host_decoder(2, true).unwrap();
        assert_eq!(hb2.targets[..], [0, 1]);
        assert_eq!(hb2.granularity, 512);
        let hb5 = plans[0].host_decoder(5, true).unwrap();
        assert_eq!((hb5.targets[..].to_vec(), hb5.granularity), (vec![3], 256));
        assert!(plans[0].host_decoder(7, true).is_none());

        let decoders = host_decoders(&plans, 5, true);
        assert_eq!(decoders.len(), 2);
        assert!(decoders[0].base < decoders[1].base);
    }

    #[cfg(feature = "acpi")]
    #[test]
    fn cfmws_restrictions_and_qtg() {
        const G: u64 = 1 << 30;
        let windows = [
            window(0x400 * G, G, Memory::Volatile, &[0]),
            window(0x500 * G, G, Memory::DeviceCoherent, &[0]),
            window(0x600 * G, G, Memory::Persistent, &[2, 0]),
        ];
        let plans = plan(&windows, target);
        let cfmws: std::vec::Vec<_> = plans.iter().map(WindowPlan::cfmws).collect();
        assert_eq!(
            cfmws
                .iter()
                .map(|c| (c.restrictions, c.qtg_id))
                .collect::<std::vec::Vec<_>>(),
            [(0x06, 1), (0x05, 2), (0x0a, 3)]
        );
        assert_eq!(cfmws[2].targets, [5, 2]);
        // The vendor's host bridge decoders are HDM-H for type-2 windows too.
        assert_eq!(
            plans[1].host_decoder(2, true).unwrap().target_type,
            fstart_pci::cxl::hdm::TargetType::HostOnly
        );
    }

    #[test]
    fn the_emulated_block_matches_the_vendor_layout() {
        const G: u64 = 1 << 30;
        let windows = [window(0x400 * G, 16 * G, Memory::Volatile, &[2])];
        let plans = plan(&windows, target);
        let decoders = host_decoders(&plans, 5, true);
        let mut shadow = std::vec![0xa5u8; SHADOW_SIZE];
        build(&mut shadow, &decoders).unwrap();
        let word = |shadow: &[u8], offset: usize| {
            u32::from_le_bytes(shadow[offset..offset + 4].try_into().unwrap())
        };
        assert_eq!(word(&shadow, 0x1000), 0x0111_0001);
        assert_eq!(word(&shadow, 0x1004), 0x0081_0005);
        assert_eq!(word(&shadow, 0x1008), 0x40); // one decoder (0), 4 targets
        assert_eq!(word(&shadow, 0x100c), 0);
        assert_eq!([word(&shadow, 0x1018), word(&shadow, 0x101c)], [0, 0x100]);
        assert_eq!([word(&shadow, 0x1020), word(&shadow, 0x1024)], [0, 4]);
        assert_eq!(word(&shadow, 0x1028), 0x1500);
        assert_eq!(word(&shadow, 0x102c), 3);
        assert_eq!(word(&shadow, 0), 0);

        // No windows: the array and an uncommitted decoder 0.
        build(&mut shadow, &[]).unwrap();
        assert_eq!(word(&shadow, 0x1008), 0x40);
        assert_eq!(word(&shadow, 0x1028), 0);
    }
}
