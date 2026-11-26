// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 fuse gating from native/mc_reg.c:meminit_mc_capability. This phase
//! accesses only host-bridge config space: MCDECS/PHY reads before initial
//! frequency/compensation setup can hang the CPU.

use super::{
    Error,
    cpu::{Cpu, Package},
    spd::Dimms,
    timings::Timings,
};
use fstart_pci::ecam::EcamDevice;
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    pub(crate) CapidA [
        SINGLE_CHANNEL OFFSET(12) NUMBITS(1) [],
        ONE_DIMM_PER_CHANNEL OFFSET(14) NUMBITS(1) [],
        DISABLE_ONE_N OFFSET(17) NUMBITS(1) [],
        MAX_CHANNEL_SIZE OFFSET(19) NUMBITS(2) [],
        VTD_DISABLED OFFSET(23) NUMBITS(1) [],
        ECC_REQUIRED OFFSET(24) NUMBITS(1) [],
        ECC_DISABLED OFFSET(25) NUMBITS(1) []
    ],
    CapidB [ DDR4_ENABLE OFFSET(3) NUMBITS(1) [] ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub capid_a: u32,
    pub capid_b: u32,
    pub capid_c: u32,
}
impl Capabilities {
    /// The shared ECAM mapping must already be bound to this stage's host
    /// bridge. No new PCI access path is introduced here.
    pub fn read() -> Result<Self, Error> {
        let sa = EcamDevice::new(0, 0, 0);
        if sa.read16(0) != 0x8086 {
            return Err(Error::HostBridgeUnavailable);
        }
        Ok(Self {
            capid_a: sa.read32(0xe4),
            capid_b: sa.read32(0xe8),
            capid_c: sa.read32(0xec),
        })
    }

    /// Keep physical channel/slot numbering when fuses disable resources.
    /// This never moves a DIMM to the other controller channel.
    pub fn apply<'a>(
        &self,
        dimms: &Dimms<'a>,
        t: &Timings,
        cpu: Cpu,
        ecc_requested: bool,
    ) -> Result<MemoryConfiguration<'a>, Error> {
        if [self.capid_a, self.capid_b, self.capid_c].contains(&u32::MAX) {
            return Err(Error::HostBridgeUnavailable);
        }
        let a = LocalRegisterCopy::<u32, CapidA::Register>::new(self.capid_a);
        let b = LocalRegisterCopy::<u32, CapidB::Register>::new(self.capid_b);
        if !b.is_set(CapidB::DDR4_ENABLE) {
            return Err(Error::Ddr4FusedOff);
        }
        let mut dimms = *dimms;
        let capacity = |channel: usize, dimms: &Dimms<'_>| {
            dimms[channel]
                .iter()
                .flatten()
                .map(|d| d.size_mib)
                .sum::<u32>()
        };
        if a.is_set(CapidA::SINGLE_CHANNEL) {
            let keep = usize::from(capacity(1, &dimms) > capacity(0, &dimms));
            dimms[keep ^ 1] = [None; 2];
        }
        if a.is_set(CapidA::ONE_DIMM_PER_CHANNEL) {
            for channel in &mut dimms {
                if let [Some(d0), Some(d1)] = *channel {
                    channel[usize::from(d1.size_mib <= d0.size_mib)] = None;
                }
            }
        }
        let maximum = match a.read(CapidA::MAX_CHANNEL_SIZE) {
            0 => 32768,
            1 => 8192,
            2 => 4096,
            _ => 2048,
        };
        for ch in 0..2 {
            if capacity(ch, &dimms) > maximum {
                return Err(Error::ChannelCapacityExceeded);
            }
        }
        if dimms.iter().flatten().all(Option::is_none) {
            return Err(Error::NoMemory);
        }
        let require_ecc = a.is_set(CapidA::ECC_REQUIRED);
        let all_ecc = dimms.iter().flatten().flatten().all(|d| d.ecc_capable);
        if require_ecc && !all_ecc {
            return Err(Error::EccRequired);
        }
        let ecc = require_ecc || (ecc_requested && !a.is_set(CapidA::ECC_DISABLED) && all_ecc);
        let khz = u64::from(t.clock.clock_khz());
        let freq = (((khz * 3 + 100000) / 200000) * 400 + 1) / 3;
        let two_n = a.is_set(CapidA::DISABLE_ONE_N)
            || freq > 2133
            || (cpu.package != Package::UltUlx && freq >= 1333);
        Ok(MemoryConfiguration {
            dimms,
            ecc,
            command_rate: if two_n { 2 } else { 1 },
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryConfiguration<'a> {
    pub dimms: Dimms<'a>,
    pub ecc: bool,
    pub command_rate: u8,
}

#[cfg(test)]
mod tests {
    use super::super::{
        spd::{
            Dimm,
            testing::{seal, sodimm},
        },
        timings,
    };
    use super::*;
    #[test]
    fn mc_capability_single_channel_and_one_dpc_keep_larger_without_renumbering() {
        let raw = sodimm();
        let mut large = raw;
        large[12] = 9;
        seal(&mut large);
        let a = Dimm::decode(&raw).unwrap();
        let b = Dimm::decode(&large).unwrap();
        let dimms = [[Some(a), None], [Some(a), Some(b)]];
        let t = timings::decode(&dimms, 938000).unwrap();
        let cpu = Cpu::from_signature(0x906e9).unwrap();
        let caps = Capabilities {
            capid_a: (1 << 12) | (1 << 14),
            capid_b: 8,
            capid_c: 0,
        };
        let memory = caps.apply(&dimms, &t, cpu, true).unwrap();
        assert_eq!(memory.dimms, [[None, None], [None, Some(b)]]);
        assert!(!memory.ecc);
        assert_eq!(memory.command_rate, 2);
    }
    #[test]
    fn mc_capability_ddr4_and_channel_capacity_fail_closed() {
        let raw = sodimm();
        let a = Dimm::decode(&raw).unwrap();
        let dimms = [[Some(a), None], [None, None]];
        let t = timings::decode(&dimms, 938000).unwrap();
        let cpu = Cpu::from_signature(0x906e9).unwrap();
        let mut caps = Capabilities {
            capid_a: 0,
            capid_b: 0,
            capid_c: 0,
        };
        assert_eq!(caps.apply(&dimms, &t, cpu, true), Err(Error::Ddr4FusedOff));
        caps.capid_b = 8;
        caps.capid_a = 2 << 19;
        assert_eq!(
            caps.apply(&dimms, &t, cpu, true),
            Err(Error::ChannelCapacityExceeded)
        );
        caps.capid_a = 1 << 19;
        assert!(caps.apply(&dimms, &t, cpu, true).is_ok());
    }
    #[test]
    fn mc_capability_ecc_required_takes_precedence_over_disabled() {
        let mut raw = sodimm();
        let dimms = [[Some(Dimm::decode(&raw).unwrap()), None], [None, None]];
        let t = timings::decode(&dimms, 938000).unwrap();
        let cpu = Cpu::from_signature(0x906e9).unwrap();
        let caps = Capabilities {
            capid_a: (1 << 24) | (1 << 25),
            capid_b: 8,
            capid_c: 0,
        };
        assert_eq!(caps.apply(&dimms, &t, cpu, false), Err(Error::EccRequired));
        raw[13] |= 8;
        seal(&mut raw);
        let dimms = [[Some(Dimm::decode(&raw).unwrap()), None], [None, None]];
        assert!(caps.apply(&dimms, &t, cpu, false).unwrap().ecc);
        let disabled = Capabilities {
            capid_a: 1 << 25,
            ..caps
        };
        assert!(!disabled.apply(&dimms, &t, cpu, true).unwrap().ecc);
    }
}
