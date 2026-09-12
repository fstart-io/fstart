use super::*;
use crate::boot::{MemoryPolicy, MemoryWindow};
use fstart_core::ffs::Compression;
use fstart_ffs::root::{BootstrapDescriptor, BootstrapRole};

#[test]
fn valid_digest_does_not_authorize_a_different_configured_entry() {
    let stored = *b"verified code";
    let mut destination = [0u8; 13];
    let address = destination.as_mut_ptr() as u64;
    let digest = fstart_crypto::digest::hash_sha256(&stored);
    let descriptor = BootstrapDescriptor {
        role: BootstrapRole::Mainstage,
        compression: Compression::None,
        offset: 0,
        stored_size: 13,
        loaded_size: 13,
        load_addr: address,
        entry_offset: 0,
        scratch_size: 0,
        stored_digest: digest,
        loaded_digest: digest,
    };
    let writable = [MemoryWindow {
        start: address,
        size: 13,
    }];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &[],
        entry_alignment: 1,
    };
    // SAFETY: source and destination are distinct live arrays; the destination
    // is exclusively owned for the complete verification operation.
    let media = unsafe { MemoryMapped::from_raw_addr(stored.as_ptr() as u64, stored.len()) };
    let executable = unsafe { crate::boot::load_bootstrap(&media, &descriptor, &policy) }.unwrap();
    let mut linux = MemoryMappedLinuxBoot::new(0, 0, 0);
    linux.kernel_addr = executable.entry();
    assert!(linux.checked_boot_params(address + 1, 0, 0, "").is_err());
    assert_eq!(
        linux
            .checked_boot_params(address, 0, 0, "")
            .unwrap()
            .kernel_addr,
        address
    );
    assert!(
        linux
            .checked_boot_params(address, address + 1, 0, "")
            .is_err()
    );
    let mut uefi = MemoryMappedUefiBoot::new(0, 0, 0);
    uefi.firmware_addr = executable.entry();
    assert!(uefi.checked_firmware_entry(address + 1).is_err());
    assert_eq!(uefi.checked_firmware_entry(address).unwrap(), address);
}
