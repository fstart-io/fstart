//! Cold-boot DDR3-1067 training, from coreboot GM45 raminit_read_write_training.c.
//! The temporary sequential 128 MiB/rank map remains active throughout.

use super::*;

const READ_PATTERN: [u32; 40] = [
    0xfefefefe, 0x7f7f7f7f, 0xbebebebe, 0xdfdfdfdf, 0xeeeeeeee, 0xf7f7f7f7, 0xfafafafa, 0xfdfdfdfd,
    0x00000000, 0x81818181, 0x40404040, 0x21212121, 0x10101010, 0x09090909, 0x04040404, 0x03030303,
    0x10101010, 0x11111111, 0xeeeeeeee, 0xefefefef, 0x10101010, 0x11111111, 0xeeeeeeee, 0xefefefef,
    0x10101010, 0xefefefef, 0x10101010, 0xefefefef, 0x10101010, 0xefefefef, 0x10101010, 0xefefefef,
    0x00000000, 0xffffffff, 0x00000000, 0xffffffff, 0x00000000, 0xffffffff, 0x00000000, 0x00000000,
];
const WRITE_PATTERN: [u32; 80] = [
    0xffffffff, 0x00000000, 0xffffffff, 0x00000000, 0xffffffff, 0x00000000, 0xffffffff, 0x00000000,
    0xffffffff, 0x00000000, 0xffffffff, 0x00000000, 0xffffffff, 0x00000000, 0xffffffff, 0x00000000,
    0xefefefef, 0x10101010, 0xefefefef, 0x10101010, 0xefefefef, 0x10101010, 0xefefefef, 0x10101010,
    0xefefefef, 0x10101010, 0xefefefef, 0x10101010, 0xefefefef, 0x10101010, 0xefefefef, 0x10101010,
    0xefefefef, 0xeeeeeeee, 0x11111111, 0x10101010, 0xefefefef, 0xeeeeeeee, 0x11111111, 0x10101010,
    0xefefefef, 0xeeeeeeee, 0x11111111, 0x10101010, 0xefefefef, 0xeeeeeeee, 0x11111111, 0x10101010,
    0x03030303, 0x04040404, 0x09090909, 0x10101010, 0x21212121, 0x40404040, 0x81818181, 0x00000000,
    0x03030303, 0x04040404, 0x09090909, 0x10101010, 0x21212121, 0x40404040, 0x81818181, 0x00000000,
    0xfdfdfdfd, 0xfafafafa, 0xf7f7f7f7, 0xeeeeeeee, 0xdfdfdfdf, 0xbebebebe, 0x7f7f7f7f, 0xfefefefe,
    0xfdfdfdfd, 0xfafafafa, 0xf7f7f7f7, 0xeeeeeeee, 0xdfdfdfdf, 0xbebebebe, 0x7f7f7f7f, 0xfefefefe,
];
const MASKS_ABC: [[u32; 2]; 4] = [
    [0x0000ffff, 0],
    [0xffff0000, 0],
    [0, 0x0000ffff],
    [0, 0xffff0000],
];
const MASKS_F: [[u32; 2]; 4] = [
    [0xff00ff00, 0],
    [0x00ff00ff, 0],
    [0, 0xff00ff00],
    [0, 0x00ff00ff],
];

// These are byte addresses in the temporary rank map, not SPD capacities.
// Never admit an empty bunch (an empty comparison would pass vacuously).
#[derive(Clone, Copy)]
pub(super) struct AddressBunch {
    addrs: [usize; 4],
    count: usize,
}
impl AddressBunch {
    const fn new() -> Self {
        Self {
            addrs: [0; 4],
            count: 0,
        }
    }
    fn push(&mut self, addr: usize) -> Result<(), ServiceError> {
        let slot = self
            .addrs
            .get_mut(self.count)
            .ok_or(ServiceError::HardwareError)?;
        *slot = addr;
        self.count += 1;
        Ok(())
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = &usize> {
        self.addrs[..self.count].iter()
    }
}

pub(super) fn rank_addresses(
    dimms: &Ddr3Channels,
    bases: &[[Option<usize>; 2]; 2],
) -> Result<[AddressBunch; 2], ServiceError> {
    let mut bunches = [AddressBunch::new(); 2];
    let mut next = 0;
    for ch in 0..2 {
        let ranks = dimms.dimms[ch].map_or(0, |d| d.ranks);
        if ranks > 2 || dimms.dimms[ch].is_some_and(|d| ddr3::validate_gm45(&d).is_err()) {
            return Err(ServiceError::HardwareError);
        }
        for rank in 0..usize::from(ranks) {
            let addr = bases[ch][rank].ok_or(ServiceError::HardwareError)?;
            if addr != next || addr > 3 * (128 << 20) {
                return Err(ServiceError::HardwareError);
            }
            bunches[ch].push(addr)?;
            next += 128 << 20;
        }
        if bases[ch][usize::from(ranks)..].iter().any(Option::is_some) {
            return Err(ServiceError::HardwareError);
        }
    }
    if next == 0 {
        return Err(ServiceError::HardwareError);
    }
    Ok(bunches)
}

fn read_offset(ch: usize, lane: usize) -> usize {
    assert!(lane < 8);
    io_channel_offset(ch, 0x14b0 + (7 - lane) * 4)
}
fn write_offset(ch: usize, group: usize) -> usize {
    assert!(group < 4);
    io_channel_offset(ch, 0x1470 + (3 - group) * 4)
}

// Represent both training positions as eighth-cycle coordinates. The hardware
// bounds, not an arbitrary iteration budget, terminate all searches.
fn read_parts(steps: i32) -> Result<(u32, u32), ServiceError> {
    if !(0..14 * 8).contains(&steps) {
        return Err(ServiceError::HardwareError);
    }
    Ok(((steps / 8) as u32, (steps % 8) as u32))
}
fn write_parts(steps: i32) -> Result<(u32, u32, u32), ServiceError> {
    if !(0..4 * 12 * 8).contains(&steps) {
        return Err(ServiceError::HardwareError);
    }
    Ok((
        (steps / (12 * 8)) as u32,
        ((steps / 8) % 12) as u32,
        (steps % 8) as u32,
    ))
}
fn set_read(mch: &Mchbar, ch: usize, lane: usize, steps: i32) -> Result<(), ServiceError> {
    let (t, p) = read_parts(steps)?;
    mch.update32(
        read_offset(ch, lane),
        (0xf << 20) | (7 << 16),
        (t << 20) | (p << 16),
    );
    Ok(())
}
fn set_write(mch: &Mchbar, ch: usize, group: usize, steps: i32) -> Result<(), ServiceError> {
    let (f, t, original_p) = write_parts(steps)?;
    // DDR3-1067: boundary correction and delay-select encoding from coreboot.
    let p = if (t == 9 && original_p >= 4) || (t == 10 && original_p < 4) {
        4
    } else {
        original_p
    };
    let d = if t <= 2 {
        3
    } else if t > 9 {
        1
    } else {
        0
    };
    mch.update32(
        write_offset(ch, group),
        (0xf << 28) | (7 << 24) | (3 << 18) | (3 << 16),
        (t << 28) | (p << 24) | (f << 18) | (d << 16),
    );
    Ok(())
}

// Coreboot's coarse search advances T, then backs up one T and scans P.
// Upper searches begin 1T (read) / 3T (write) beyond lower; both must
// start inside the good eye. An unbounded edge is an error, never a success.
fn find_eye(
    start: i32,
    stride: i32,
    upper_offset: i32,
    limit: i32,
    mut program: impl FnMut(i32) -> Result<(), ServiceError>,
    mut good: impl FnMut() -> bool,
) -> Result<i32, ServiceError> {
    let mut lower = start;
    loop {
        program(lower)?;
        if good() {
            break;
        }
        lower += stride;
        if lower >= limit {
            return Err(ServiceError::HardwareError);
        }
    }
    if lower >= stride {
        lower -= stride;
        loop {
            program(lower)?;
            if good() {
                break;
            }
            lower += 1;
            if lower >= limit {
                return Err(ServiceError::HardwareError);
            }
        }
    }
    let mut upper = lower + upper_offset;
    program(upper)?;
    if !good() {
        return Err(ServiceError::HardwareError);
    }
    loop {
        upper += stride;
        if upper >= limit {
            return Err(ServiceError::HardwareError);
        }
        program(upper)?;
        if !good() {
            break;
        }
    }
    upper -= stride;
    loop {
        program(upper)?;
        if !good() {
            break;
        }
        upper += 1;
        if upper >= limit {
            return Err(ServiceError::HardwareError);
        }
    }
    Ok((lower + upper) / 2)
}

// The first temporary rank starts at physical address zero. Rust's volatile
// pointer operations require non-null pointers even when page zero is mapped;
// use x86 memory operands for these bounded training-pattern accesses.
#[inline(always)]
fn read_training_word(address: usize) -> u32 {
    let value: u32;
    // SAFETY: callers use populated temporary rank addresses and aligned offsets
    // inside the identity-mapped DRAM test region.
    unsafe {
        core::arch::asm!(
            "mov {value:e}, dword ptr [{address}]",
            address = in(reg) address,
            value = out(reg) value,
            options(nostack, preserves_flags),
        );
    }
    value
}

#[inline(always)]
fn write_training_word(address: usize, value: u32) {
    // SAFETY: callers use populated temporary rank addresses and aligned offsets
    // inside the identity-mapped DRAM test region.
    unsafe {
        core::arch::asm!(
            "mov dword ptr [{address}], {value:e}",
            address = in(reg) address,
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

fn read_test(lane: usize, addresses: &AddressBunch) -> bool {
    let word_offset = lane & 4;
    let mask = 0xff << ((lane & 3) * 8);
    addresses.iter().all(|&addr| {
        (word_offset..320).step_by(8).all(|off| {
            let value = read_training_word(addr + off);
            value & mask == READ_PATTERN[off / 8] & mask
        })
    })
}
fn train_read(mch: &Mchbar, ch: usize, addresses: &AddressBunch) -> Result<(), ServiceError> {
    for &addr in addresses.iter() {
        for off in (0..320).step_by(4) {
            write_training_word(addr + off, READ_PATTERN[off / 8]);
        }
    }
    for lane in 0..8 {
        mch.update32(read_offset(ch, lane), 0, 3 << 25);
        let mean = find_eye(
            0,
            8,
            8,
            14 * 8,
            |steps| set_read(mch, ch, lane, steps),
            || read_test(lane, addresses),
        )?;
        set_read(mch, ch, lane, mean)?;
    }
    Ok(())
}

fn write_test(mch: &Mchbar, addresses: &AddressBunch, masks: [u32; 2]) -> bool {
    let regs = mch.regs();
    let arbitration = regs.misc1.get();
    let control = regs.misc0.get();
    regs.misc1.set(arbitration | (0xf << 28));
    regs.misc0.set(control | (1 << 4));
    let mut good = true;
    'ranks: for &addr in addresses.iter() {
        for off in (0..640).step_by(8) {
            let pattern = WRITE_PATTERN[off / 8];
            write_training_word(addr + off, pattern);
            write_training_word(addr + off + 4, pattern);
        }
        regs.training_read_control
            .set(regs.training_read_control.get() | 1);
        for off in (0..640).step_by(8) {
            let pattern = WRITE_PATTERN[off / 8];
            let first = read_training_word(addr + off);
            let second = read_training_word(addr + off + 4);
            if first & masks[0] != pattern & masks[0] || second & masks[1] != pattern & masks[1] {
                good = false;
                break 'ranks;
            }
        }
    }
    regs.misc1.set(arbitration);
    regs.misc0.set(control);
    good
}
fn train_write(
    mch: &Mchbar,
    ch: usize,
    addresses: &AddressBunch,
    card_f: bool,
) -> Result<(), ServiceError> {
    let masks = if card_f { &MASKS_F } else { &MASKS_ABC };
    for (group, &mask) in masks.iter().enumerate() {
        let seed = mch.read32(write_offset(ch, group));
        // Coreboot seeds from existing F/T/P fields, starting one F earlier.
        let f = ((seed >> 2) & 3) as i32 - 1;
        let t = ((seed >> 12) & 0xf) as i32;
        let p = ((seed >> 8) & 7) as i32;
        if t >= 12 {
            return Err(ServiceError::HardwareError);
        }
        let mean = find_eye(
            (f * 12 + t) * 8 + p,
            8,
            24,
            4 * 12 * 8,
            |steps| set_write(mch, ch, group, steps),
            || write_test(mch, addresses, mask),
        )?;
        set_write(mch, ch, group, mean)?;
    }
    Ok(())
}
fn reset_pointers(mch: &Mchbar) {
    let regs = mch.regs();
    for channel in mch.post_jedec_channels() {
        channel
            .pointer_control
            .set(channel.pointer_control.get() | (1 << 6));
        channel
            .pointer_control
            .set(channel.pointer_control.get() & !(1 << 6));
    }
    for ptr in [&regs.write_pointer0, &regs.write_pointer1] {
        ptr.set(ptr.get() & !(1 << 9));
        ptr.set(ptr.get() | (1 << 9));
        ptr.set(ptr.get() | (1 << 10));
    }
}

pub(super) fn read_write_training(
    mch: &Mchbar,
    dimms: &Ddr3Channels,
    bases: &[[Option<usize>; 2]; 2],
) -> Result<(), ServiceError> {
    let addresses = rank_addresses(dimms, bases)?;
    for (ch, bunch) in addresses.iter().enumerate() {
        if bunch.count > 0 {
            train_read(mch, ch, bunch)?;
        }
    }
    reset_pointers(mch);
    let card_f = dimms.dimms.map(|d| d.is_some_and(|d| d.raw_card == 5));
    if addresses[0].count > 0 && card_f[0] == card_f[1] {
        // Same raw-card layout: channel A groups test every populated rank.
        let mut combined = addresses[0];
        for &addr in addresses[1].iter() {
            combined.push(addr)?;
        }
        train_write(mch, 0, &combined, card_f[0])?;
    } else {
        for (ch, bunch) in addresses.iter().enumerate() {
            if bunch.count > 0 {
                train_write(mch, ch, bunch, card_f[ch])?;
            }
        }
    }
    reset_pointers(mch);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gm45_training_register_offsets_and_patterns() {
        assert_eq!(read_offset(0, 0), 0x14cc);
        assert_eq!(read_offset(1, 7), 0x15b0);
        assert_eq!(write_offset(0, 0), 0x147c);
        assert_eq!(write_offset(1, 3), 0x1570);
        assert_eq!(READ_PATTERN[39], 0);
        assert_eq!(
            (WRITE_PATTERN[0], WRITE_PATTERN[16], WRITE_PATTERN[79]),
            (0xffffffff, 0xefefefef, 0xfefefefe)
        );
        assert_eq!(
            core::mem::offset_of!(PreJedecRegs, training_read_control),
            0x78
        );
        assert_eq!(
            core::mem::offset_of!(PostJedecChannelRegs, pointer_control),
            0xc
        );
    }
    #[test]
    fn gm45_training_math_and_missing_edges_fail_closed() {
        assert_eq!(read_parts(13 * 8 + 7), Ok((13, 7)));
        assert!(read_parts(14 * 8).is_err());
        assert!(read_parts(-1).is_err());
        assert_eq!(write_parts(2 * 96 + 9 * 8 + 5), Ok((2, 9, 5)));
        assert!(write_parts(-1).is_err());
        assert!(write_parts(4 * 96).is_err());
        let at = core::cell::Cell::new(0);
        let eye = find_eye(
            0,
            8,
            8,
            112,
            |s| {
                at.set(s);
                Ok(())
            },
            || (11..=45).contains(&at.get()),
        );
        assert_eq!(eye, Ok(28)); // lower 11, upper 46
        let at = core::cell::Cell::new(0);
        assert!(
            find_eye(
                0,
                8,
                8,
                112,
                |s| {
                    at.set(s);
                    Ok(())
                },
                || at.get() >= 11
            )
            .is_err()
        );
        assert!(find_eye(0, 8, 8, 112, |_| Ok(()), || false).is_err());
    }
    #[test]
    fn gm45_training_rank_bunch_rejects_invalid_topology() {
        let dimms = Ddr3Channels {
            dimms: [None, None],
        };
        assert!(rank_addresses(&dimms, &[[None; 2]; 2]).is_err());
    }
}
