// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exercise the production Immediate Command state machine, not MMIO offsets
//! or write snapshots. Model only BUSY/RW1C VALID and a bounded clock.
use super::*;
use core::cell::Cell;
use tock_registers::registers::InMemoryRegister;
const STALE: u32 = 0xdead_dead;
const FRESH: u32 = 0x10ec_0233;
struct Engine {
    raw: Cell<u16>,
    started: Cell<bool>,
    elapsed: Cell<u32>,
    valid_at: Option<u32>,
    done_at: Option<u32>,
    response: Cell<u32>,
    reads: Cell<u32>,
    writes: Cell<u32>,
}
impl Engine {
    fn new(raw: u16, valid_at: Option<u32>, done_at: Option<u32>) -> Self {
        Self {
            raw: Cell::new(raw),
            started: Cell::new(false),
            elapsed: Cell::new(0),
            valid_at,
            done_at,
            response: Cell::new(STALE),
            reads: Cell::new(0),
            writes: Cell::new(0),
        }
    }
}
struct Status<'a>(&'a Engine);
impl Readable for Status<'_> {
    type T = u16;
    type R = ICS::Register;
    fn get(&self) -> u16 {
        self.0.raw.get()
    }
}
impl Writeable for Status<'_> {
    type T = u16;
    type R = ICS::Register;
    fn set(&self, value: u16) {
        self.0.writes.set(self.0.writes.get() + 1);
        self.0.raw.set(
            (self.0.raw.get() & ICS::VALID::SET.value & !(value & ICS::VALID::SET.value))
                | (value & ICS::BUSY::SET.value),
        );
        self.0.started.set(value & ICS::BUSY::SET.value != 0);
    }
}
struct Response<'a>(&'a Engine);
impl Readable for Response<'_> {
    type T = u32;
    type R = ();
    fn get(&self) -> u32 {
        self.0.reads.set(self.0.reads.get() + 1);
        self.0.response.get()
    }
}
struct Clock<'a>(&'a Engine);
impl DelayNs for Clock<'_> {
    fn delay_ns(&mut self, ns: u32) {
        let e = self.0;
        e.elapsed.set(e.elapsed.get() + ns / 1000);
        if e.started.get() {
            if e.valid_at.is_some_and(|t| e.elapsed.get() >= t) {
                e.raw.set(e.raw.get() | ICS::VALID::SET.value);
            }
            if e.done_at.is_some_and(|t| e.elapsed.get() >= t) {
                e.response.set(FRESH);
                e.raw.set(e.raw.get() & !ICS::BUSY::SET.value);
            }
        }
    }
}
#[test]
fn immediate_command_does_not_accept_stale_or_early_valid_while_busy() {
    let engine = Engine::new(ICS::VALID::SET.value, Some(5), Some(20));
    let command = InMemoryRegister::<u32>::new(0);
    assert_eq!(
        immediate_command(
            &command,
            &Response(&engine),
            &Status(&engine),
            0x100f0000,
            &mut Clock(&engine)
        ),
        Ok(FRESH)
    );
    assert_eq!(engine.elapsed.get(), 20);
    assert_eq!(engine.reads.get(), 1);
}
#[test]
fn immediate_command_rejects_busy_owner_without_starting_or_aborting_it() {
    let engine = Engine::new(ICS::BUSY::SET.value, None, None);
    let command = InMemoryRegister::<u32>::new(STALE);
    assert_eq!(
        immediate_command(
            &command,
            &Response(&engine),
            &Status(&engine),
            0x100f0000,
            &mut Clock(&engine)
        ),
        Err(ControllerError::Busy)
    );
    assert_eq!(engine.elapsed.get(), 1000);
    assert_eq!(engine.writes.get(), 0);
    assert_eq!(command.get(), STALE);
    assert_eq!(engine.reads.get(), 0);
}
#[test]
fn immediate_command_busy_clear_without_valid_is_not_a_response() {
    let engine = Engine::new(0, None, Some(10));
    let command = InMemoryRegister::<u32>::new(0);
    assert_eq!(
        immediate_command(
            &command,
            &Response(&engine),
            &Status(&engine),
            0x100f0000,
            &mut Clock(&engine)
        ),
        Err(ControllerError::InvalidResponse)
    );
    assert_eq!(engine.reads.get(), 0);
}
#[test]
fn immediate_command_timeout_is_bounded_and_abort_does_not_ack_late_valid() {
    let engine = Engine::new(0, Some(5), None);
    let command = InMemoryRegister::<u32>::new(0);
    assert_eq!(
        immediate_command(
            &command,
            &Response(&engine),
            &Status(&engine),
            0x100f0000,
            &mut Clock(&engine)
        ),
        Err(ControllerError::CommandTimeout)
    );
    assert_eq!(engine.elapsed.get(), 1000);
    assert_eq!(engine.reads.get(), 0);
    assert_eq!(engine.raw.get() & ICS::BUSY::SET.value, 0);
    assert_ne!(engine.raw.get() & ICS::VALID::SET.value, 0);
}
