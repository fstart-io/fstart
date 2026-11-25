// SPDX-License-Identifier: MIT OR Apache-2.0
//! Discrete TIS TPM population retention across S3.
//!
//! The cold TPM population is part of the OS's retained ACPI contract.
//! Absence is recorded too so a missing or swapped part rejects S3 resume
//! cleanly rather than continuing with stale ACPI tables.
//!
//! State is persisted in [`fstart_store::tag::TPM_STATE`] inside the RAM-backed
//! firmware store rather than CMOS, avoiding board NVRAM/checksum overlap and
//! surviving S3 alongside the retained ACPI tables.

use fstart_driver_tpm::{
    Family,
    tis::{Identity, Irq},
};
use fstart_store::{Store, tag::TPM_STATE};

const RECORD_SIZE: usize = 8;

/// Outcome of probing and starting a discrete LPC/MMIO TIS TPM during ramstage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TisProbe {
    pub family: Option<Family>,
    pub irq: Option<Irq>,
}

fn encode(id: Option<Identity>, irq: Option<Irq>) -> [u8; RECORD_SIZE] {
    let didvid = id.map_or(0, |i| u32::from(i.device) << 16 | u32::from(i.vendor));
    let info = id.map_or(0, |i| {
        u32::from(i.revision) << 8 | if i.family == Family::Tpm12 { 1 } else { 2 }
    }) | irq.map_or(0, |i| {
        u32::from(i.vector) << 16 | u32::from(i.low) << 24 | u32::from(i.edge) << 25
    });
    let mut record = [0u8; RECORD_SIZE];
    record[..4].copy_from_slice(&didvid.to_le_bytes());
    record[4..].copy_from_slice(&info.to_le_bytes());
    record
}

/// Persist the cold TPM identity and IRQ route in [`TPM_STATE`], or verify that
/// an S3 resume observes the exact same population without rewriting the entry.
pub fn same_population(
    store: &mut Store,
    resume: bool,
    id: Option<Identity>,
    irq: Option<Irq>,
) -> bool {
    let expected = encode(id, irq);
    if resume {
        let Some(entry) = store.find(TPM_STATE) else {
            return false;
        };
        // SAFETY: validated store entry, exclusively accessed on the BSP.
        unsafe { store.bytes_mut(&entry) == expected.as_slice() }
    } else {
        let entry = match store.find(TPM_STATE) {
            Some(entry) if entry.len() == RECORD_SIZE => entry,
            Some(_) => return false,
            None => match store.add(TPM_STATE, RECORD_SIZE, 3) {
                Ok(entry) => entry,
                Err(_) => return false,
            },
        };
        // SAFETY: fresh or matched store entry, exclusively accessed on the BSP before sealing.
        unsafe { store.bytes_mut(&entry) }.copy_from_slice(&expected);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(64))]
    struct Window([u8; 512]);

    fn fresh_store(window: &mut Window, size: usize) -> Store {
        // SAFETY: aligned writable backing buffer owned for the test's lifetime.
        unsafe { Store::create(core::ptr::NonNull::from(&mut window.0).cast(), size).unwrap() }
    }

    #[test]
    fn tpm_state_round_trip_and_mismatch_rejection() {
        let mut window = Window([0; 512]);
        let mut store = fresh_store(&mut window, 512);
        let id = Identity {
            family: Family::Tpm12,
            vendor: 0x15d1,
            device: 0x0007,
            revision: 1,
        };
        let irq = Irq {
            vector: 5,
            low: true,
            edge: false,
        };

        // Resume without a recorded cold entry must fail.
        assert!(!same_population(&mut store, true, None, None));
        assert!(!same_population(&mut store, true, Some(id), Some(irq)));

        // Cold boot records TPM_STATE; matching resume succeeds without growing store.
        assert!(same_population(&mut store, false, Some(id), Some(irq)));
        let used = store.used();
        assert!(same_population(&mut store, true, Some(id), Some(irq)));
        assert_eq!(store.used(), used);

        // Missing, replaced, or re-routed TPM must reject resume and leave entry intact.
        assert!(!same_population(&mut store, true, None, None));
        assert!(!same_population(
            &mut store,
            true,
            Some(Identity { revision: 2, ..id }),
            Some(irq),
        ));
        assert!(!same_population(
            &mut store,
            true,
            Some(Identity {
                family: Family::Tpm20,
                ..id
            }),
            Some(irq),
        ));
        assert!(!same_population(&mut store, true, Some(id), None));
        assert!(!same_population(
            &mut store,
            true,
            Some(id),
            Some(Irq { vector: 6, ..irq }),
        ));
        assert!(same_population(&mut store, true, Some(id), Some(irq)));
        assert_eq!(store.used(), used);

        // Cold boot with no TPM records absence; resume agrees only when still absent.
        let mut absent_window = Window([0; 512]);
        let mut absent_store = fresh_store(&mut absent_window, 512);
        assert!(same_population(&mut absent_store, false, None, None));
        assert!(same_population(&mut absent_store, true, None, None));
        assert!(!same_population(&mut absent_store, true, Some(id), None));
    }

    #[test]
    fn full_store_withholds_s3_cleanly() {
        let mut window = Window([0; 512]);
        // Only room for the 24-byte Transfer List header, no room for an entry.
        let mut store = fresh_store(&mut window, fstart_store::HEADER_SIZE);
        assert!(!same_population(&mut store, false, None, None));
        assert_eq!(store.find(TPM_STATE), None);
    }
}
