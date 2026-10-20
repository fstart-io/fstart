use super::*;
extern crate std;
use crate::fam17h::tsc::testing::Delay;
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
    buffer: *mut u8,
    /// The requests as published.
    requests: Vec<Vec<u8>>,
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
            let header = self.buffer.cast::<Header>().read();
            let request = std::slice::from_raw_parts(self.buffer, header.size.get() as usize);
            assert_eq!(header.status.get(), 0);
            self.requests.push(request.to_vec());
            // A query reply: the PSP fills in the value.
            let reply = Wire {
                header: Header {
                    size: self.size.into(),
                    status: self.status.into(),
                },
                value: 0x1234.into(),
                reserved: [0; 20],
            };
            let bytes = if header.size.get() == 32 {
                reply.as_bytes()
            } else {
                &reply.as_bytes()[..8]
            };
            self.buffer.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        }
    }
}
fn mailbox(reads: &[u32], fallback: u32, size: u32, status: u32) -> Mailbox<Mock> {
    let buffer = Box::leak(Box::new(Buffer::new()));
    let bytes = buffer.bytes.get().cast::<u8>();
    // Private protocol test construction bypasses real identity addresses and
    // global one-attempt claim. Public constructor never accepts this fake map.
    Mailbox {
        io: Mock {
            reads: reads.iter().copied().collect(),
            fallback,
            writes: Vec::new(),
            buffer: bytes,
            requests: Vec::new(),
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
    let mut delay = Delay(|_| panic!("no waiting expected"));
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
    let query = [&32u32.to_le_bytes()[..], &[0; 28]].concat();
    assert!(mailbox.io.requests.iter().all(|request| *request == query));
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
            mailbox.capabilities(&mut Delay(|us| {
                assert_eq!(us, 100);
                delays += 1;
            })),
            Err(error)
        );
        assert_eq!(delays, expected_delays);
        assert!(mailbox.io.writes.is_empty());
        assert_eq!(
            mailbox.capabilities(&mut Delay(|_| panic!("poisoned"))),
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
        assert_eq!(mailbox.capabilities(&mut Delay(|_| {})), Err(error));
        assert_eq!(mailbox.io.writes.len(), 2);
        assert_eq!(
            mailbox.ftpm_capabilities(&mut Delay(|_| panic!("poisoned"))),
            Err(Error::Poisoned)
        );
        assert_eq!(
            unsafe { mailbox.boot_done(&mut Delay(|_| panic!("poisoned"))) },
            Err(Error::Poisoned)
        );
        assert_eq!(mailbox.io.writes.len(), 2);
    }
}
#[test]
fn boot_done_is_a_terminal_non_smm_transition_not_a_boot_claim() {
    let mut mailbox = mailbox(&[], READY, 32, 0);
    // Host fake only: no PSP or SMM state changed by this test.
    assert_eq!(unsafe { mailbox.boot_done(&mut Delay(|_| {})) }, Ok(()));
    assert_eq!(mailbox.io.writes[1], Event::Submit(0x06 << 16));
    assert_eq!(
        mailbox.capabilities(&mut Delay(|_| panic!("closed"))),
        Err(Error::BootDone)
    );
    assert_eq!(
        unsafe { mailbox.boot_done(&mut Delay(|_| panic!("closed"))) },
        Err(Error::BootDone)
    );
    assert_eq!(mailbox.io.writes.len(), 2);
}
#[test]
fn smm_info_has_the_vendor_gen2_layout() {
    let register = |address, mask, value| SmmRegister {
        address,
        mask,
        value,
    };
    let info = SmmInfo {
        tseg_base: 0x7f80_0000,
        tseg_mask: 0xffff_ff80_0000,
        p2c_buffer: 0x7f81_0000,
        p2c_size: 0x1000,
        registers: SmiRegisters {
            trigger: register(0xfed8_0298, 1 << 25, 1 << 25),
            smi_enable: register(0xfed8_0298, 1 << 31, 0),
            eos: register(0xfed8_0298, 1 << 28, 1 << 28),
            psp_smi_enable: register(0xfed8_02a8, 0xc, 0x4),
        },
        c2p_buffer: 0x7f81_1000,
        smm_flag: 0x7f81_1c00,
    };
    let mut mailbox = mailbox(&[], READY, 0x110, 0);
    assert_eq!(mailbox.smm_info(&info, &mut Delay(|_| {})), Ok(()));
    assert_eq!(mailbox.io.writes[1], Event::Submit(0x02 << 16));
    let wire = &mailbox.io.requests[0];
    let u32_at = |offset: usize| u32::from_le_bytes(wire[offset..offset + 4].try_into().unwrap());
    let u64_at = |offset: usize| u64::from_le_bytes(wire[offset..offset + 8].try_into().unwrap());
    assert_eq!((u32_at(0), u32_at(4)), (0x110, 0));
    // Offsets of the request, which starts after the 8-byte header.
    let req = |offset: usize| 8 + offset;
    assert_eq!(u64_at(req(0x00)), 0x7f80_0000);
    assert_eq!(u64_at(req(0x08)), 0xffff_ff80_0000);
    assert_eq!((u64_at(req(0x10)), u64_at(req(0x18))), (0x7f81_0000, 0x1000));
    // Trigger: memory dword, AND and OR masks.
    assert_eq!(u64_at(req(0x20)), 0xfed8_0298);
    assert_eq!(
        [req(0x28), req(0x2c), req(0x30), req(0x34)].map(u32_at),
        [1, 2, !(1 << 25), 1 << 25]
    );
    // SmiEnb, EOS and the PSP SMI enable: mask and expected value.
    assert_eq!([req(0x48), req(0x4c)].map(u32_at), [1 << 31, 0]);
    assert_eq!([req(0x60), req(0x64)].map(u32_at), [1 << 28, 1 << 28]);
    assert_eq!(u64_at(req(0x68)), 0xfed8_02a8);
    assert_eq!([req(0x78), req(0x7c)].map(u32_at), [0xc, 0x4]);
    assert!(wire[req(0x80)..req(0xf8)].iter().all(|&byte| byte == 0));
    assert_eq!((u64_at(req(0xf8)), u64_at(req(0x100))), (0x7f81_1000, 0x7f81_1c00));
}
#[test]
fn complete_allocation_preflight_precedes_one_way_claim() {
    assert_eq!(size_of::<Buffer>(), 0x120);
    assert_eq!(core::mem::align_of::<Buffer>(), 32);
    for start in [0, 1, 0x02000001, u64::MAX, 0x1_0000_0000] {
        assert_eq!(placement(start), Err(Error::Placement));
    }
    assert_eq!(placement(0x1_0000_0000 - 0x120), Ok(()));
    assert_eq!(placement(0xffffffe0), Err(Error::Placement));
    assert_eq!(placement(0x02000000), Ok(()));
}
