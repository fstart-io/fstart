extern crate std;
use super::*;
use crate::smn::testing::SmnSpace;
use core::cell::RefCell;
use std::vec::Vec as StdVec;

fn policy() -> Policy {
    Policy {
        enabled: [true, true],
        over_current: [0xf0f0, 0xffff],
        device_removable: 0,
        ras: false,
        over_current_low: true,
        sparse: true,
        debug_pipe_switch: false,
        force_gen1: 0xf,
        usb2: None,
        usb3: None,
    }
}
struct Messages<'a> {
    calls: RefCell<StdVec<(u32, [u32; 6])>>,
    fail: Option<usize>,
    guard_after: Option<(usize, &'a SmnSpace, u32)>,
}
impl Messages<'_> {
    fn new() -> Self {
        Self {
            calls: RefCell::new(StdVec::new()),
            fail: None,
            guard_after: None,
        }
    }
}
impl SmuMessages for Messages<'_> {
    fn send(&self, id: u32, args: &mut [u32; 6]) -> Result<(), smu::Error> {
        let mut calls = self.calls.borrow_mut();
        calls.push((id, *args));
        if let Some((n, hw, value)) = self.guard_after {
            if n == calls.len() {
                hw.set(GUARD, value);
            }
        }
        if self.fail == Some(calls.len()) {
            Err(smu::Error::CompletionTimeout)
        } else {
            Ok(())
        }
    }
}
fn tuning() -> Usb3Tuning {
    Usb3Tuning {
        word_1242d8: 7,
        bit_12c064: false,
        nibble_12c064: 0,
        word_13004c: 7,
        bit_120088: true,
        mid_120088: 5,
        low_bit_120088: false,
        low_120088: 5,
    }
}

#[test]
fn genoa_absolute_register_stream_has_no_nfx_reset_or_phy_operations() {
    use crate::nbio::iohc::{NB_PCI_CTRL, NbPciCtrl};
    use tock_registers::interfaces::Readable;
    let hw = SmnSpace::default();
    hw.set(GUARD, (1 << 9) | (1 << 10)); // NFX guard semantics are not Genoa policy.
    let smu = Messages::new();
    let mut boot = ColdBoot::for_interface(&policy(), Interface::Absolute).unwrap();
    boot.run(&hw.smn(), &smu).unwrap();
    let calls = smu.calls.borrow();
    assert_eq!(calls.len(), 17);
    assert_eq!(
        calls[0],
        (0x13, [0x16d80120, 0xffff, 0xf0f0, 0x01000001, 0, 0])
    );
    assert_eq!(
        calls[1],
        (0x13, [0x16f80120, 0xffff, 0xffff, 0x01100001, 0, 0])
    );
    assert_eq!(
        calls[12],
        (0x13, [0x16c0d024, 0x400, 0x400, 0x01000003, 0, 0])
    );
    assert_eq!(
        calls[15],
        (0x13, [0x16e0d0a4, 0x400, 0x400, 0x01100003, 0, 0])
    );
    assert_eq!(calls[16], (0xa, [3, 0, 0, 0, 0, 0]));
    assert!(hw.smn().reg(NB_PCI_CTRL).is_set(NbPciCtrl::USB_PME_DISABLE));
    assert!(
        hw.smn()
            .reg(NB_PCI_CTRL.offset(0x200000))
            .is_set(NbPciCtrl::USB_PME_DISABLE)
    );
    assert_eq!(boot.run(&hw.smn(), &smu), Err(Error::Consumed));
    let mut p = policy();
    p.usb3 = Some([[tuning(); 2]; 2]);
    assert!(matches!(
        ColdBoot::for_interface(&p, Interface::Absolute),
        Err(Error::Policy)
    ));
}

#[test]
fn native_nfx_scalar_stream_not_sp3_absolute_register_abi() {
    let hw = SmnSpace::default();
    let smu = Messages::new();
    let mut p = policy();
    p.usb3 = Some([[tuning(); 2]; 2]);
    let mut boot = ColdBoot::new(&p).unwrap();
    assert_eq!(boot.steps.len(), 36);
    boot.run(&hw.smn(), &smu).unwrap();
    let calls = smu.calls.borrow();
    assert_eq!(calls[0], (0x13, [0, 0, 0, 0xd0000000, 0, 0]));
    assert_eq!(
        calls[1],
        (0x13, [0x180120, 0xffff, 0xf0f0, 0x01000001, 0, 0])
    );
    assert_eq!(
        calls[2],
        (0x13, [0x180120, 0xffff, 0xffff, 0x01100001, 0, 0])
    );
    assert_eq!(calls[11], (0x13, [0x180130, 0xf000, 0, 0x01000001, 0, 0]));
    assert_eq!(calls[12], (0x13, [0x1242d8, 0x7f, 7, 0x01000002, 0, 0]));
    assert_eq!(calls[15], (0x13, [0x120088, 0xff, 0xd5, 0x01000002, 0, 0]));
    assert_eq!(calls[33], (0x13, [0, 0, 0, 0xc0000002, 0, 0]));
    assert_eq!(calls[34], (0x13, [0, 0, 0, 0xc0100002, 0, 0]));
    assert_eq!(calls[35], (0xa, [3, 0, 0, 0, 0, 0]));
    assert!(calls.iter().all(|(_, a)| a[4] == 0 && a[5] == 0));
    assert_eq!(boot.run(&hw.smn(), &smu), Err(Error::Consumed));
}
#[test]
fn usb2_coefficients_and_controller_groups_are_endian_independent() {
    let mut p = policy();
    p.usb2 = Some([[Usb2Tuning([3, 3, 3, 3, 0, 1, 6, 3, 1]); 2]; 2]);
    let boot = ColdBoot::new(&p).unwrap();
    assert_eq!(
        boot.steps[11].args,
        [0x11c000, 0xf3bf7007, 0x61333003, 0x01000001, 0, 0]
    );
    assert_eq!(boot.steps[12].args, [0x11c004, 0xf, 7, 0x01000001, 0, 0]);
    assert_eq!(
        boot.steps[15].args,
        [0x11c000, 0xf3bf7007, 0x61333003, 0x01100001, 0, 0]
    );
    p.usb3 = Some([[tuning(); 2]; 2]);
    assert_eq!(ColdBoot::new(&p).unwrap().steps.len(), 44);
}
#[test]
fn validates_every_coefficient_before_any_hardware_access() {
    let mut p = policy();
    let mut t = tuning();
    t.word_1242d8 = 128;
    p.usb3 = Some([[t; 2]; 2]);
    assert!(matches!(ColdBoot::new(&p), Err(Error::Policy)));
    p.usb3 = None;
    p.usb2 = Some([[Usb2Tuning([8, 0, 0, 0, 0, 0, 0, 0, 0]); 2]; 2]);
    assert!(matches!(ColdBoot::new(&p), Err(Error::Policy)));
    p.usb2 = None;
    p.device_removable = 1 << 31;
    assert!(matches!(ColdBoot::new(&p), Err(Error::Policy)));
    p.device_removable = 0;
    p.enabled = [false, false];
    assert!(matches!(ColdBoot::new(&p), Err(Error::Policy)));
}
#[test]
fn all_access_guards_fail_closed_and_consume_attempt() {
    for (value, error) in [(u32::MAX, Error::Unavailable), (1 << 9, Error::Disabled)] {
        let hw = SmnSpace::default();
        hw.set(GUARD, value);
        let smu = Messages::new();
        let mut boot = ColdBoot::new(&policy()).unwrap();
        assert_eq!(boot.run(&hw.smn(), &smu), Err(error));
        assert!(smu.calls.borrow().is_empty());
        assert_eq!(boot.run(&hw.smn(), &smu), Err(Error::Consumed));
    }
}
#[test]
fn rechecks_subsequence_guard_after_parameter_messages() {
    for value in [1 << 10, u32::MAX] {
        let hw = SmnSpace::default();
        let mut smu = Messages::new();
        smu.guard_after = Some((13, &hw, value));
        let mut boot = ColdBoot::new(&policy()).unwrap();
        let result = boot.run(&hw.smn(), &smu);
        if value == u32::MAX {
            assert_eq!(result, Err(Error::Unavailable));
            assert_eq!(smu.calls.borrow().len(), 13);
        } else {
            result.unwrap();
            assert_eq!(smu.calls.borrow().len(), 14);
            assert_eq!(smu.calls.borrow().last().unwrap().0, INIT_DONE);
        }
    }
}
#[test]
fn timeout_stops_before_more_messages_and_is_not_retried() {
    let hw = SmnSpace::default();
    let mut smu = Messages::new();
    smu.fail = Some(4);
    let mut boot = ColdBoot::new(&policy()).unwrap();
    assert_eq!(
        boot.run(&hw.smn(), &smu),
        Err(Error::Smu(smu::Error::CompletionTimeout))
    );
    assert_eq!(smu.calls.borrow().len(), 4);
    assert_eq!(boot.run(&hw.smn(), &smu), Err(Error::Consumed));
    assert_eq!(smu.calls.borrow().len(), 4);
}
#[test]
fn disabled_controller_visibility_and_done_mask_follow_prepared_policy() {
    let hw = SmnSpace::default();
    for reg in VISIBILITY {
        hw.set(reg, u32::MAX);
    }
    let mut p = policy();
    p.enabled = [false, true];
    let smu = Messages::new();
    let mut boot = ColdBoot::new(&p).unwrap();
    boot.run(&hw.smn(), &smu).unwrap();
    assert_eq!(hw.get(VISIBILITY[0]), u32::MAX & !(1 << 28));
    assert_eq!(hw.get(VISIBILITY[1]), u32::MAX);
    assert_eq!(smu.calls.borrow().last().unwrap().1, [2, 0, 0, 0, 0, 0]);
}
