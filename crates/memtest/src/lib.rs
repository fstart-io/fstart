//! DRAM tests for board bring-up, run from a bootblock on freshly trained
//! memory.

#![no_std]

use core::ptr::{read_volatile, write_volatile};

/// First mismatching word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    pub addr: usize,
    pub expected: u32,
    pub actual: u32,
}

/// Which half of a pass is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Write,
    Verify,
}

/// Progress through [`own_address`]: `words` of the range are done in
/// `phase` of pass `pass` (0 stores the addresses, 1 their complements).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub pass: u8,
    pub phase: Phase,
    pub words: usize,
}

/// Own-address test: store each word's address in it, then verify the
/// whole range; then the same with the complement. Catches aliased address
/// lines (a word holding another word's address) and data bits stuck at
/// either level. Every phase covers the whole range, so aliasing between
/// any two words shows up; `progress` hears after every `step` words and
/// at the end of each phase.
///
/// # Safety
///
/// `[base, base + 4 * words)` must be RAM that nothing else uses during the
/// test; its contents are destroyed.
pub unsafe fn own_address(
    base: *mut u32,
    words: usize,
    step: usize,
    mut progress: impl FnMut(Progress),
) -> Result<(), Fault> {
    let step = step.max(1);
    for (pass, invert) in [(0u8, 0), (1, u32::MAX)] {
        for phase in [Phase::Write, Phase::Verify] {
            let mut done = 0;
            while done < words {
                let end = words.min(done.saturating_add(step));
                // SAFETY: `done..end` lies in the caller's range.
                unsafe {
                    match phase {
                        Phase::Write => write(base, done, end, invert),
                        Phase::Verify => verify(base, done, end, invert)?,
                    }
                }
                done = end;
                progress(Progress {
                    pass,
                    phase,
                    words: done,
                });
            }
        }
    }
    Ok(())
}

/// Store each word's address, XOR `invert`, in words `from..to`. The loop
/// makes no calls, so nothing in it reloads constants: a bootblock running
/// from flash with the D-cache off would read each from flash.
///
/// # Safety
///
/// `base[from..to]` must be the caller's RAM.
unsafe fn write(base: *mut u32, from: usize, to: usize, invert: u32) {
    for i in from..to {
        // SAFETY: in bounds per the caller's contract.
        unsafe {
            let p = base.add(i);
            write_volatile(p, p as usize as u32 ^ invert);
        }
    }
}

/// Check words `from..to` hold what [`write`] stored.
///
/// # Safety
///
/// `base[from..to]` must be the caller's RAM.
unsafe fn verify(base: *mut u32, from: usize, to: usize, invert: u32) -> Result<(), Fault> {
    for i in from..to {
        // SAFETY: in bounds per the caller's contract.
        let (p, actual) = unsafe {
            let p = base.add(i);
            (p, read_volatile(p))
        };
        let expected = p as usize as u32 ^ invert;
        if actual != expected {
            return Err(Fault {
                addr: p as usize,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    #[test]
    fn clean_memory_passes_and_ends_inverted() {
        let mut buf = [0u32; 256];
        let base = buf.as_mut_ptr();
        assert_eq!(unsafe { own_address(base, buf.len(), 64, |_| {}) }, Ok(()));
        let last = &buf[255] as *const u32 as usize as u32;
        assert_eq!(buf[255], !last);
    }

    #[test]
    fn each_phase_spans_the_whole_range_before_the_next() {
        let mut buf = [0u32; 100];
        let mut seen = Vec::new();
        let r = unsafe { own_address(buf.as_mut_ptr(), buf.len(), 40, |p| seen.push(p)) };
        assert_eq!(r, Ok(()));
        let at = |pass, phase, words| Progress { pass, phase, words };
        let mut expected = Vec::new();
        for pass in 0..2 {
            for phase in [Phase::Write, Phase::Verify] {
                for words in [40, 80, 100] {
                    expected.push(at(pass, phase, words));
                }
            }
        }
        assert_eq!(seen, expected);
    }
}
