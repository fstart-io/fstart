use super::*;
extern crate std;
use std::{boxed::Box, collections::VecDeque, vec, vec::Vec};
const READY: u32 = 1 << 31;
#[derive(Debug, PartialEq)]
enum Event {
    Pointer(u64),
    Submit(u32),
}
struct Mock {
    reads: VecDeque<u32>,
    fallback: u32,
    writes: Vec<Event>,
    wire: *mut Wire,
    size: u32,
    status: u32,
}
// SAFETY: host-only synchronous fake DMA finishes inside submit; no hardware.
unsafe impl Transport for Mock {
    fn command(&mut self) -> u32 {
        self.reads.pop_front().unwrap_or(self.fallback)
    }
    unsafe fn buffer_pointer(&mut self, address: u64) {
        self.writes.push(Event::Pointer(address));
    }
    unsafe fn submit(&mut self, value: u32) {
        self.writes.push(Event::Submit(value));
        unsafe {
            let mut wire = self.wire.read();
            assert_eq!(wire.size.get(), 32);
            assert_eq!(wire.status.get(), 0);
            assert_eq!(wire.value.get(), 0);
            assert_eq!(wire.reserved, [0; 20]);
            wire.size = self.size.into();
            wire.status = self.status.into();
            wire.value = 0x1234.into();
            self.wire.write(wire);
        }
    }
}
fn mailbox(reads: &[u32], fallback: u32, size: u32, status: u32) -> Mailbox<Mock> {
    let buffer = Box::leak(Box::new(Buffer::new()));
    let wire = buffer.wire.get().cast::<Wire>();
    // Private protocol test construction bypasses real identity addresses and
    // global one-attempt claim. Public constructor never accepts this fake map.
    Mailbox {
        io: Mock {
            reads: reads.iter().copied().collect(),
            fallback,
            writes: Vec::new(),
            wire,
            size,
            status,
        },
        buffer,
        address: 0x02000000,
        state: State::Ready,
        not_send_sync: core::marker::PhantomData,
    }
}
#[test]
fn query_commands_publish_zero_padded_buffer_pointer_before_command() {
    let mut mailbox = mailbox(&[], READY, 32, 0);
    let mut delay = |_| panic!("no waiting expected");
    assert_eq!(mailbox.capabilities(&mut delay), Ok(0x1234));
    assert_eq!(mailbox.ftpm_capabilities(&mut delay), Ok(0x1234));
    assert_eq!(mailbox.hsti_state(&mut delay), Ok(0x1234));
    assert_eq!(mailbox.lock_df_registers(&mut delay), Ok(()));
    assert_eq!(
        mailbox.io.writes,
        vec![
            Event::Pointer(0x02000000),
            Event::Submit(0x27 << 16),
            Event::Pointer(0x02000000),
            Event::Submit(0x05 << 16),
            Event::Pointer(0x02000000),
            Event::Submit(0x14 << 16),
            Event::Pointer(0x02000000),
            Event::Submit(0x1b << 16)
        ]
    );
    // Stale delivery status is cleared, not copied into the trigger register.
    let mut mailbox = self::mailbox(&[READY | 7, READY], READY, 32, 0);
    assert_eq!(mailbox.capabilities(&mut delay), Ok(0x1234));
    assert_eq!(mailbox.io.writes[1], Event::Submit(0x27 << 16));
}
#[test]
fn pre_submission_unavailable_recovery_and_timeout_poison_without_writes() {
    for (value, error, expected_delays) in [
        (u32::MAX, Error::Unavailable, 0),
        (READY | (1 << 30), Error::Recovery, 0),
        (READY | (1 << 16), Error::ReadyTimeout, POLLS),
    ] {
        let mut mailbox = mailbox(&[], value, 32, 0);
        let mut delays = 0;
        assert_eq!(
            mailbox.capabilities(&mut |us| {
                assert_eq!(us, 100);
                delays += 1;
            }),
            Err(error)
        );
        assert_eq!(delays, expected_delays);
        assert!(mailbox.io.writes.is_empty());
        assert_eq!(
            mailbox.capabilities(&mut |_| panic!("poisoned")),
            Err(Error::Poisoned)
        );
    }
}
#[test]
fn submitted_failures_retain_storage_and_cannot_retry_or_boot_done() {
    for (response, size, status, error) in [
        (READY | 3, 32, 0, Error::Delivery(3)),
        (u32::MAX, 32, 0, Error::Unavailable),
        (READY | (1 << 30), 32, 0, Error::Recovery),
        (READY, 12, 0, Error::BufferSize(12)),
        (READY, 32, 9, Error::BufferStatus(9)),
        (0, 32, 0, Error::CompletionTimeout),
    ] {
        let mut mailbox = mailbox(&[READY], response, size, status);
        assert_eq!(mailbox.capabilities(&mut |_| {}), Err(error));
        assert_eq!(mailbox.io.writes.len(), 2);
        assert_eq!(
            mailbox.ftpm_capabilities(&mut |_| panic!("poisoned")),
            Err(Error::Poisoned)
        );
        assert_eq!(
            unsafe { mailbox.boot_done(&mut |_| panic!("poisoned")) },
            Err(Error::Poisoned)
        );
        assert_eq!(mailbox.io.writes.len(), 2);
    }
}
#[test]
fn boot_done_is_a_terminal_non_smm_transition_not_a_boot_claim() {
    let mut mailbox = mailbox(&[], READY, 32, 0);
    // Host fake only: no PSP or SMM state changed by this test.
    assert_eq!(unsafe { mailbox.boot_done(&mut |_| {}) }, Ok(()));
    assert_eq!(mailbox.io.writes[1], Event::Submit(0x06 << 16));
    assert_eq!(
        mailbox.capabilities(&mut |_| panic!("closed")),
        Err(Error::BootDone)
    );
    assert_eq!(
        unsafe { mailbox.boot_done(&mut |_| panic!("closed")) },
        Err(Error::BootDone)
    );
    assert_eq!(mailbox.io.writes.len(), 2);
}
#[test]
fn complete_allocation_preflight_precedes_one_way_claim() {
    assert_eq!(size_of::<Buffer>(), 32);
    assert_eq!(core::mem::align_of::<Buffer>(), 32);
    for start in [0, 1, 0x02000001, u64::MAX, 0x1_0000_0000] {
        assert_eq!(placement(start), Err(Error::Placement));
    }
    assert_eq!(placement(0xffffffe0), Ok(()));
    assert_eq!(placement(0x02000000), Ok(()));
    let flag = AtomicBool::new(false);
    assert_eq!(claim(&flag), Ok(()));
    assert_eq!(claim(&flag), Err(Error::AlreadyAttempted));
}
