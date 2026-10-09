extern crate std;

use super::*;
use std::boxed::Box;

#[repr(C, align(4096))]
struct Window([u8; 0x4000]);

fn window() -> Box<Window> {
    Box::new(Window([0xa5; 0x4000]))
}

fn ptr(window: &mut Window, offset: usize) -> NonNull<u8> {
    NonNull::new(window.0[offset..].as_mut_ptr()).unwrap()
}

#[test]
fn entries_are_found_with_aligned_data() {
    let mut mem = window();
    let mut store = unsafe { Store::create(ptr(&mut mem, 0), 0x4000) }.unwrap();
    let stamps = store.add(tag::TIMESTAMPS, 100, 3).unwrap();
    let acpi = store.add(tag::ACPI, 0x200, 12).unwrap();
    let smbios = store.add(tag::SMBIOS, 5, 4).unwrap();

    assert_eq!(store.address(&acpi) % 4096, 0);
    assert_eq!(store.address(&smbios) % 16, 0);
    assert_eq!(store.find(tag::ACPI), Some(acpi));
    assert_eq!(store.find(tag::CONSOLE), None);
    assert_eq!(
        store.entries().collect::<std::vec::Vec<_>>(),
        [stamps, acpi, smbios]
    );
    unsafe { store.bytes_mut(&acpi) }.fill(0x11);
    assert_eq!(unsafe { store.bytes_mut(&stamps) }.len(), 100);
    // The padding before the ACPI data did not swallow the timestamp data.
    assert!(unsafe { store.bytes_mut(&acpi) }.iter().all(|b| *b == 0x11));
}

#[test]
fn reopening_validates_the_list() {
    let mut mem = window();
    let base = ptr(&mut mem, 0);
    let mut store = unsafe { Store::create(base, 0x4000) }.unwrap();
    let entry = store.add(tag::TRAINING, 64, 3).unwrap();

    let reopened = unsafe { Store::open(base, 0x4000) }.unwrap();
    assert_eq!(reopened.find(tag::TRAINING), Some(entry));
    assert_eq!(
        unsafe { Store::open(base, 0x1000) }.err(),
        Some(Error::TooSmall)
    );

    // Corrupt entry chain: data size runs past the used size.
    mem.0[HEADER_SIZE + 4] = 0xff;
    assert_eq!(
        unsafe { Store::open(base, 0x4000) }.err(),
        Some(Error::Invalid)
    );
    mem.0[0] ^= 1;
    assert_eq!(
        unsafe { Store::open(base, 0x4000) }.err(),
        Some(Error::Invalid)
    );
}

#[test]
fn limit_caps_growth_at_the_used_size() {
    let mut mem = window();
    let base = ptr(&mut mem, 0);
    let mut store = unsafe { Store::create(base, 0x4000) }.unwrap();
    store.add(tag::STAGE_CACHE_POSTCAR, 0x1100, 3).unwrap();
    assert_eq!(store.limit(0x1000), 0x2000);
    assert_eq!(store.add(tag::CONSOLE, 0x1000, 3), Err(Error::Full));
    // The limit persists and fits a window that ends at the reservation.
    let reopened = unsafe { Store::open(base, 0x2000) }.unwrap();
    assert_eq!(reopened.max_size(), 0x2000);
}

#[test]
fn only_the_last_entry_resizes() {
    let mut mem = window();
    let mut store = unsafe { Store::create(ptr(&mut mem, 0), 0x4000) }.unwrap();
    let first = store.add(tag::ACPI, 0x2000, 12).unwrap();
    let second = store.add(tag::SMBIOS, 0x100, 3).unwrap();
    assert_eq!(store.resize_last(first, 0x10), Err(Error::NotLast));
    let used = store.used();
    let shrunk = store.resize_last(second, 0x10).unwrap();
    assert_eq!(store.used(), used - 0xf0);
    assert_eq!(store.find(tag::SMBIOS), Some(shrunk));
    assert_eq!(store.resize_last(shrunk, 0x4000), Err(Error::Full));
    let next = store.add(tag::CONSOLE, 8, 3).unwrap();
    assert_eq!(store.address(&next), store.address(&shrunk) + 0x10 + 8);
}

#[test]
fn relocation_keeps_entry_offsets() {
    let mut early = window();
    let mut late = window();
    let mut car = unsafe { Store::create(ptr(&mut early, 0), 0x400) }.unwrap();
    let stamps = car.add(tag::TIMESTAMPS, 0x40, 3).unwrap();
    unsafe { car.bytes_mut(&stamps) }.fill(0x42);
    assert_eq!(car.add(tag::CONSOLE, 0x400, 3), Err(Error::Full));

    let mut dram = unsafe { car.relocate(ptr(&mut late, 0), 0x4000) }.unwrap();
    assert_eq!(dram.find(tag::TIMESTAMPS), Some(stamps));
    assert!(
        unsafe { dram.bytes_mut(&stamps) }
            .iter()
            .all(|b| *b == 0x42)
    );
    dram.add(tag::CONSOLE, 0x400, 3).unwrap();
}

#[test]
fn alignment_is_checked_against_the_base() {
    let mut mem = window();
    let mut store = unsafe { Store::create(ptr(&mut mem, 8), 0x1000) }.unwrap();
    assert_eq!(store.add(tag::ACPI, 16, 12), Err(Error::Misaligned));
    store.add(tag::TIMESTAMPS, 16, 3).unwrap();
    let mut aligned = unsafe { Store::create(ptr(&mut mem, 0x1000), 0x2000) }.unwrap();
    aligned.add(tag::ACPI, 16, 12).unwrap();
    let mut other = window();
    assert_eq!(
        unsafe { aligned.relocate(ptr(&mut other, 8), 0x2000) }.err(),
        Some(Error::Misaligned)
    );
    assert_eq!(store.add(Tag::VOID, 8, 3), Err(Error::BadTag));
}
