// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exclusive RAM-backed PCI devices, not physical SBI/endpoint validation.
extern crate std;
use super::*;
use fstart_pci::{PciAddress, PciRoot, PciRootInfo, ecam_host::PciEcam};
use std::{vec, vec::Vec};

fn ecam() -> (PciEcam, Vec<u32>) {
    let mut image = vec![0xa5a5_a5a5; (1 << 20) / 4];
    let pci = PciEcam::new([PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: image.as_mut_ptr() as u64,
            bus_start: 0,
            bus_end: 0,
        },
        [],
    )
    .unwrap()])
    .unwrap();
    (pci, image)
}
fn message() -> Message {
    Message {
        port: 0xcf,
        offset: 0xbeef,
        opcode: Opcode::PcrRead,
        posted: false,
        byte_enable: 15,
        bar: 0,
        function: 0,
    }
}
fn frame(device: &EcamDevice) -> [u32; 5] {
    [
        device.read32(ADDRESS),
        device.read32(EXTENDED_ADDRESS),
        u32::from(device.read16(STATUS)),
        u32::from(device.read16(ROUTE)),
        device.read32(DATA),
    ]
}
struct Device<'a> {
    device: &'a EcamDevice,
    response: u16,
    complete: bool,
    previous: usize,
    calls: usize,
    start: Option<[u32; 5]>,
}
impl DelayNs for Device<'_> {
    fn delay_ns(&mut self, ns: u32) {
        assert_eq!(ns, 1_000);
        self.calls += 1;
        let status = unsafe { self.device.register::<u16, Status::Register>(STATUS) };
        assert!(status.is_set(Status::BUSY));
        if self.previous != 0 {
            // Even a successfully completed previous wait must not hide early
            // writes to the staging registers while the engine was busy.
            assert_eq!(
                frame(self.device),
                [0xa5a5_a5a5, 0xa5a5_a5a5, 1, 0xa5a5, 0xa5a5_a5a5]
            );
            self.previous -= 1;
            if self.previous == 0 {
                status.modify(Status::BUSY::CLEAR);
            }
            return;
        }
        self.start.get_or_insert_with(|| frame(self.device));
        if self.complete {
            let op = status.read(Status::OPCODE);
            if self.response == 0 && matches!(op, 0 | 4 | 6) {
                self.device.write32(
                    DATA,
                    0x6d00_0000
                        ^ self.device.read32(ADDRESS)
                        ^ u32::from(self.device.read16(ROUTE)),
                );
            }
            status.modify(Status::BUSY::CLEAR + Status::RESPONSE.val(self.response));
        }
    }
}

#[test]
fn p2sb_preexisting_busy_is_waited_before_reprogramming() {
    let (pci, _image) = ecam();
    let device = pci.device(PciAddress::new(0, 0, 0x1f, 1)).unwrap();
    device.write16(0, 0x8086);
    device.write16(STATUS, 1);
    let mut model = Device {
        device: &device,
        response: 0,
        complete: true,
        previous: 3,
        calls: 0,
        start: None,
    };
    let mut sbi = unsafe { Sbi::new(&device).unwrap() };
    let mut data = 0x1234_5678;
    assert_eq!(sbi.execute(message(), &mut data, &mut model), Ok(()));
    assert_eq!(model.calls, 4);
    assert_eq!(model.start.unwrap()[0], 0xcf00_beef);
    assert_eq!(data, 0x6d00_0000 ^ 0xcf00_beef ^ 0xf000);
}

#[test]
fn p2sb_busy_timeouts_are_bounded_and_started_timeout_poisoned() {
    for previous in [true, false] {
        let (pci, image) = ecam();
        let device = pci.device(PciAddress::new(0, 0, 0x1f, 1)).unwrap();
        device.write16(0, 0x8086);
        device.write16(STATUS, u16::from(previous));
        let before = image.clone();
        let mut model = Device {
            device: &device,
            response: 0,
            complete: false,
            previous: if previous { POLLS + 1 } else { 0 },
            calls: 0,
            start: None,
        };
        let mut sbi = unsafe { Sbi::new(&device).unwrap() };
        let mut data = 0x1234_5678;
        assert_eq!(
            sbi.execute(message(), &mut data, &mut model),
            Err(if previous {
                Error::PreviousBusyTimeout
            } else {
                Error::CompletionTimeout
            })
        );
        assert_eq!(model.calls, POLLS);
        assert_eq!(data, 0x1234_5678);
        if previous {
            assert_eq!(image, before);
            assert!(model.start.is_none());
        } else {
            assert!(model.start.is_some());
            assert_eq!(device.read16(STATUS) & 1, 1);
            let after = image.clone();
            assert_eq!(
                sbi.execute(message(), &mut data, &mut model),
                Err(Error::Poisoned)
            );
            assert_eq!(image, after);
            assert_eq!(model.calls, POLLS);
        }
    }
}
