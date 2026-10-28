//! Rome/Milan USB: the x86 side only asks the SMU to program the two xHCI
//! controllers (FCH_SP3 §3.1 af48). Each socket's list goes to that
//! socket's SMU.

use super::super::{FchHw, hide_nbif_function, update};
use crate::sp3::nbio::nbif::func_strap0;
use crate::sp3::smu::{SmuError, SmuMessages};
use crate::sp3::soc::Generation;

/// SMU V11 messages (`smu::v11::msg`).
const USB_CONFIG_UPDATE: u32 = 0x13;
const USB_INIT_DONE: u32 = 0x0A;
const USB_S3_RESUME_DONE: u32 = 0x15;

/// The two xHCI functions: NBIF1 device 0 function 3 of quadrants 0 and 1
/// (`1053_4600h`, `1063_4600h`; FCH_SP3 §3.1), hidden before the SMU
/// programs the controllers.
const XHCI_STRAPS: [u32; 2] = [
    0x1050_0000 + func_strap0(0, 3),
    0x1060_0000 + func_strap0(0, 3),
];
/// USB0/USB1 over-current pin maps (DeviceRemovable/OC map registers).
const OC_MAP: [u32; 2] = [0x16D8_0120, 0x16F8_0120];

/// Board USB policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbPolicy {
    /// Over-current maps written to USB0/USB1 `0x120` (mask FFFFh).
    pub oc_map: [u32; 2],
}

impl UsbPolicy {
    /// Donor defaults per generation and socket (FCH_SP3 §3.1, §4.2, §8).
    pub const fn default_for(generation: Generation, socket: u8) -> Self {
        let oc_map = match (generation, socket) {
            (Generation::Rome, 0) => [0xFFFF, 0x3232],
            (Generation::Rome, _) => [0x1010, 0x3F3F],
            _ => [0xFFFF, 0xFFFF],
        };
        Self { oc_map }
    }
}

fn requests(generation: Generation) -> &'static [(u32, u32, u32, u32)] {
    match generation {
        #[cfg(feature = "rome")]
        Generation::Rome => super::super::tables::USB_CONFIG_ROME,
        #[cfg(feature = "milan")]
        Generation::Milan => super::super::tables::USB_CONFIG_MILAN,
        g => super::super::common::not_built(g),
    }
}

/// PEI af48 on one socket. `smu` is that socket's SMU; `resume` selects
/// S3ResumeDone instead of UsbInitDone. Socket 1 additionally sets
/// `16D8014C[31]` first (FCH_SP3 §4.2).
pub fn reset<H: FchHw, S: SmuMessages + ?Sized>(
    hw: &H,
    generation: Generation,
    socket: u8,
    smu: &S,
    policy: &UsbPolicy,
    resume: bool,
) -> Result<(), SmuError> {
    let smn = hw.smn_window();
    if socket != 0 {
        update::<u32, _>(&smn, 0x16D8_014C, 0, 1 << 31);
    }
    XHCI_STRAPS
        .iter()
        .for_each(|&r| hide_nbif_function(&smn, r));
    requests(generation)
        .iter()
        .try_for_each(|&(reg, mask, data, group)| {
            let data = OC_MAP
                .iter()
                .position(|&r| r == reg)
                .map_or(data, |i| policy.oc_map[i]);
            smu.send(USB_CONFIG_UPDATE, &mut [reg, mask, data, group, 0, 0])
        })?;
    let done = if resume {
        USB_S3_RESUME_DONE
    } else {
        USB_INIT_DONE
    };
    smu.send(done, &mut [0; 6])
}

#[cfg(test)]
mod tests {
    use super::super::super::model::{Smu, fch, smn};
    use super::*;

    #[cfg(feature = "rome")]
    #[test]
    fn rome_socket1_list() {
        let hw = fch();
        let smu = Smu::default();
        reset(
            &hw,
            Generation::Rome,
            1,
            &smu,
            &UsbPolicy::default_for(Generation::Rome, 1),
            false,
        )
        .unwrap();
        let sent = smu.0.borrow();
        assert_eq!(sent.len(), 11);
        // FCH_SP3 §4.2: socket-1 OC maps 1010/3F3F, then UsbInitDone.
        assert_eq!(sent[2].1[..4], [0x16d8_0120, 0xffff, 0x1010, 0x0100_0001]);
        assert_eq!(sent[3].1[2], 0x3f3f);
        assert_eq!(sent[10].0, 0x0a);
        assert_eq!(smn(&hw.pci, 0, 0x16d8_014c), 1 << 31);
    }

    #[cfg(feature = "milan")]
    #[test]
    fn milan_s3_resume() {
        let hw = fch();
        let smu = Smu::default();
        reset(
            &hw,
            Generation::Milan,
            0,
            &smu,
            &UsbPolicy::default_for(Generation::Milan, 0),
            true,
        )
        .unwrap();
        let ids = smu.ids();
        assert_eq!(ids.len(), 19);
        assert_eq!(*ids.last().unwrap(), 0x15);
        assert_eq!(smn(&hw.pci, 0, 0x16d8_014c), 0);
    }
}
