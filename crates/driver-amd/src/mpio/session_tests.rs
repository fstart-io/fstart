use super::*;
use crate::smn::testing::SmnSpace;
use fstart_core::services::{ManualTimer, TimerDelay};
use mailbox::{ARGUMENTS, CONTROL, Control, DOORBELL, msg};

// Model-only metadata, never hardware register definitions.
const RAM_LO: u32 = 0xffff0000;
const RAM_HI: u32 = RAM_LO + 4;
const MODE: u32 = RAM_LO + 8;
const COUNT: u32 = RAM_LO + 12;
const PHASE: u32 = RAM_LO + 16;
const COMMANDS: u32 = RAM_LO + 20;
#[repr(align(64))]
struct Ram([u8; DMA_SIZE]);

fn firmware(hw: &SmnSpace, address: u32, _: u32) {
    if address != DOORBELL.address() {
        return;
    }
    hw.set(COMMANDS, hw.get(COMMANDS) + 1);
    let command = Control::REQUEST.read(hw.get(CONTROL)) as u16;
    let args = core::array::from_fn::<_, 6, _>(|i| hw.get(ARGUMENTS.offset(i as u32 * 4)));
    if hw.get(MODE) == 4 && command == msg::TRANSFER_ASK {
        return;
    } // unresolved pointer-bearing op
    let ram = (u64::from(hw.get(RAM_LO)) | (u64::from(hw.get(RAM_HI)) << 32)) as usize as *mut u8;
    match command {
        msg::GLOBAL_CONFIG => assert_eq!(
            args,
            [if hw.get(MODE) == 6 { 0xc1 } else { 0xc0 }, 0, 0, 0, 0, 0]
        ),
        msg::EXT_ATTRIBUTES => assert_eq!(args, [0, 0x9000, 7, 0, 0, 0]),
        msg::TRANSFER_ASK => {
            assert_eq!(
                (args[0], args[1], args[2], args[3], args[5]),
                (0, 0x1000, 3, 0, 0)
            );
            hw.set(COUNT, args[4]);
        }
        msg::SETUP_LINK | 4 => {
            hw.set(PHASE, args[0]);
        }
        msg::GET_ASK => {
            assert_eq!(args, [0, 0x1000, 0, 0, 0, 0]);
            let state: u32 = match hw.get(PHASE) {
                1 => 1,
                6 => 2,
                8 => 4,
                0x30 => 6,
                _ => 0,
            };
            for i in 0..hw.get(COUNT) as usize {
                // SAFETY: the model's live reserved backing region, no
                // asynchronous writer; command completion follows this copy.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        state.to_le_bytes().as_ptr(),
                        ram.add(i * ASK_BYTES + 0x20),
                        4,
                    );
                }
            }
            if hw.get(MODE) == 1 {
                unsafe {
                    ram.add(DMA_SIZE - 1).write(0);
                }
            }
        }
        msg::STATUS | msg::UPDATE_LINK | 0x22 => {}
        _ => panic!("unexpected request {command:x}"),
    }
    hw.set(
        ARGUMENTS,
        if command == msg::STATUS || (hw.get(MODE) == 3 && command == msg::TRANSFER_ASK) {
            0
        } else {
            1
        },
    );
    hw.set(
        CONTROL,
        if hw.get(MODE) == 5 && command == msg::TRANSFER_ASK {
            0x800000fe
        } else {
            0x80000001
        },
    );
}
fn coherent_model(_: *const u8, len: usize) {
    assert_eq!(len, DMA_SIZE);
}
fn poll() -> Poll {
    Poll {
        reads: NonZeroU32::new(2).unwrap(),
        interval_us: NonZeroU32::new(1).unwrap(),
    }
}
fn setup(ram: *mut u8) -> SmnSpace {
    let mut hw = SmnSpace::default();
    hw.on_write = Some(firmware);
    hw.set(RAM_LO, ram as usize as u32);
    hw.set(RAM_HI, ((ram as usize as u64) >> 32) as u32);
    hw.set(CONTROL, 0x80000000);
    hw
}
fn pcie() -> Config<'static> {
    Config {
        port: Port::pcie(0, 3),
        phy: &[],
        speed: &[],
    }
}

#[test]
fn genoa_low_byte_encoding_retains_the_same_phase_and_dma_contract() {
    let mut ram = std::boxed::Box::new(Ram([0; DMA_SIZE]));
    let ptr = ram.0.as_mut_ptr();
    let hw = setup(ptr);
    let prepared = Prepared::new(0, &[0], &[pcie()])
        .unwrap()
        .with_encoding(mailbox::Encoding::LowByte);
    let dma = unsafe { Dma::new(ptr, DMA_SIZE, 0x1000) }.unwrap();
    let mut session = unsafe {
        Session::new(
            hw.smn(),
            dma,
            prepared,
            coherent_model,
            poll(),
            NonZeroU32::new(3).unwrap(),
        )
    }
    .unwrap();
    let timer = ManualTimer::new();
    let mut delay = TimerDelay(&timer);
    session.submit(&mut delay).unwrap();
    for _ in 0..4 {
        session.advance(&mut delay).unwrap();
    }
    assert_eq!(session.state(), State::Trained);
    session.update(&[Some(0)], &mut delay).unwrap();
    assert_eq!(session.state(), State::Updated);
}

#[test]
fn explicit_onyx_vetting_policy_changes_only_global_bit_zero() {
    let mut ram = std::boxed::Box::new(Ram([0; DMA_SIZE]));
    let ptr = ram.0.as_mut_ptr();
    let hw = setup(ptr);
    hw.set(MODE, 6);
    let prepared = Prepared::new(0, &[0], &[pcie()])
        .unwrap()
        .with_psp_vetting(false);
    let dma = unsafe { Dma::new(ptr, DMA_SIZE, 0x1000) }.unwrap();
    let mut session = unsafe {
        Session::new(
            hw.smn(),
            dma,
            prepared,
            coherent_model,
            poll(),
            NonZeroU32::new(3).unwrap(),
        )
    }
    .unwrap();
    session
        .submit(&mut TimerDelay(&ManualTimer::new()))
        .unwrap();
    assert_eq!(session.state(), State::Submitted);
}

#[test]
fn complete_wire_sequence_and_snapshots_without_claiming_hardware_training() {
    let mut ram = std::boxed::Box::new(Ram([0; DMA_SIZE]));
    let ptr = ram.0.as_mut_ptr();
    let hw = setup(ptr);
    let mut sata = Port::pcie(92, 95);
    sata.kind = rs::Kind::Sata;
    let prepared = Prepared::new(
        0,
        &[0, 2, 3],
        &[
            pcie(),
            Config {
                port: sata,
                phy: &[],
                speed: &[0x1c07, 0x2107, 0x08002807],
            },
        ],
    )
    .unwrap();
    let dma = unsafe { Dma::new(ptr, DMA_SIZE, 0x1000) }.unwrap();
    // SAFETY: synchronous, coherent host model; the backing RAM outlives
    // this controller and it has no asynchronous access after teardown.
    let mut session = unsafe {
        Session::new(
            hw.smn(),
            dma,
            prepared,
            coherent_model,
            poll(),
            NonZeroU32::new(3).unwrap(),
        )
    }
    .unwrap();
    assert_eq!(
        session.advance(&mut TimerDelay(&ManualTimer::new())),
        Err(Error::State)
    );
    session
        .submit(&mut TimerDelay(&ManualTimer::new()))
        .unwrap();
    for (phase, status) in [
        (State::Mapped, 1),
        (State::Configured, 2),
        (State::Perst, 4),
        (State::Trained, 6),
    ] {
        session
            .advance(&mut TimerDelay(&ManualTimer::new()))
            .unwrap();
        assert_eq!(session.state(), phase);
        assert_eq!(session.asks().unwrap()[0].training_state(), status);
    }
    assert_eq!(
        session.update(&[Some(2), Some(1)], &mut TimerDelay(&ManualTimer::new())),
        Err(Error::Buffer)
    );
    session
        .update(&[Some(2), None], &mut TimerDelay(&ManualTimer::new()))
        .unwrap();
    assert_eq!(session.state(), State::Updated);
    assert_eq!(session.asks().unwrap()[0].bytes()[0x16] & 31, 2);
}

#[test]
fn bad_guard_or_unresolved_dma_poison_and_cannot_be_bypassed() {
    for mode in [1, 3, 4, 5] {
        let mut ram = std::boxed::Box::new(Ram([0; DMA_SIZE]));
        let ptr = ram.0.as_mut_ptr();
        let hw = setup(ptr);
        hw.set(MODE, mode);
        let prepared = Prepared::new(0, &[0, 2, 3], &[pcie()]).unwrap();
        let dma = unsafe { Dma::new(ptr, DMA_SIZE, 0x1000) }.unwrap();
        let mut session = unsafe {
            Session::new(
                hw.smn(),
                dma,
                prepared,
                coherent_model,
                poll(),
                NonZeroU32::new(3).unwrap(),
            )
        }
        .unwrap();
        let error = if mode == 1 {
            session
                .submit(&mut TimerDelay(&ManualTimer::new()))
                .unwrap();
            session.advance(&mut TimerDelay(&ManualTimer::new()))
        } else {
            session.submit(&mut TimerDelay(&ManualTimer::new()))
        };
        assert_eq!(
            error,
            Err(match mode {
                1 => Error::Guard,
                3 => Error::Receipt(msg::TRANSFER_ASK, 0),
                4 => Error::Mailbox(mailbox::Error::CompletionTimeout),
                5 => Error::Mailbox(mailbox::Error::Rejected(0xfe)),
                _ => unreachable!(),
            })
        );
        assert_eq!(session.state(), State::Poisoned);
        let commands = hw.get(COMMANDS);
        assert!(session.asks().is_err());
        assert_eq!(
            session.advance(&mut TimerDelay(&ManualTimer::new())),
            Err(Error::State)
        );
        assert_eq!(
            session.submit(&mut TimerDelay(&ManualTimer::new())),
            Err(Error::State)
        );
        assert_eq!(hw.get(COMMANDS), commands);
        // The synchronous model has now stopped; no pointer can be reused
        // before real firmware ownership is re-established/reset on hardware.
    }
}

#[test]
fn preflight_rejects_empty_overlap_fused_off_or_excessive_ports() {
    assert!(matches!(
        Prepared::new(0, &[0, 2, 3], &[]),
        Err(Error::Empty)
    ));
    assert!(matches!(
        Prepared::new(0, &[0, 2, 3], &[pcie(); 33]),
        Err(Error::Capacity)
    ));
    assert!(matches!(
        Prepared::new(0, &[0, 2, 3], &[pcie(), pcie()]),
        Err(Error::Overlap)
    ));
    let mut hotplug = pcie();
    hotplug.port.hotplug = 1;
    assert!(matches!(
        Prepared::new(0, &[0, 2, 3], &[hotplug]),
        Err(Error::Unsupported)
    ));
    let mut disabled = pcie();
    disabled.port = Port::pcie(32, 47);
    assert!(matches!(
        Prepared::new(0, &[0, 2, 3], &[disabled]),
        Err(Error::Lanes(_))
    ));
}
