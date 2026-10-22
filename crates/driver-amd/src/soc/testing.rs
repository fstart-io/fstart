// SPDX-License-Identifier: MIT OR Apache-2.0
//! A host model of the dies for whole-sequence write logs of the Family 17h
//! silicon init: PCI config space, SMN behind D0F0 0xb8/0xbc, the DF FICAA
//! pair, an SMU with a scripted DXIO firmware and the FCH register spaces.
//! It logs every write and delay in order. Unset registers read as noise,
//! so a read-modify-write and a plain write differ.
extern crate std;
use crate::{
    ccx::tsc::{Clock, DelayNs},
    fch::{Bus, Space},
};
use core::cell::RefCell;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use std::{collections::BTreeMap, vec::Vec};

const MESSAGE: u32 = 0x03b1_0528;
const RESPONSE: u32 = 0x03b1_0564;
const SMN_INDEX: u16 = 0xb8;
const SMN_DATA: u16 = 0xbc;
const FICAA: u16 = 0x5c;
const FICAD: u16 = 0x98;
/// D0F0 of a root bus is the NB configuration space in SMN.
const NB_CONFIG: u32 = 0x13b0_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Config(PciAddress, u16, u32),
    Fch(Space, u32, u8, u32),
    Delay(u32),
}
impl Event {
    fn words(self) -> [u64; 4] {
        match self {
            Self::Config(a, offset, value) => [
                1,
                u64::from(a.bus()) << 16 | u64::from(a.device()) << 8 | u64::from(a.function()),
                offset.into(),
                value.into(),
            ],
            Self::Fch(space, address, bytes, value) => [
                2,
                (space as u64) << 8 | u64::from(bytes),
                address.into(),
                value.into(),
            ],
            Self::Delay(us) => [3, 0, 0, us.into()],
        }
    }
}

fn noise(words: &[u64]) -> u32 {
    let mut x = words.iter().fold(0x9e37_79b9_7f4a_7c15u64, |h, &w| {
        (h ^ w).wrapping_mul(0xbf58_476d_1ce4_e5b9)
    });
    x ^= x >> 31;
    (x.wrapping_mul(0x94d0_49bb_1331_11eb) >> 16) as u32
}

/// The DXIO state replies after Start: resume, mapped, PERST, configured,
/// an error event, an unknown event, done.
const DXIO_EVENTS: [[u32; 3]; 8] = [
    [3, 1, 0],
    [3, 2, 0x0f],
    [3, 4, 1],
    [3, 2, 0x05],
    [2, 4, 0],
    [3, 9, 0],
    [3, 4, 0],
    [3, 2, 0x0d],
];

#[derive(Default)]
struct Dxio {
    config: Vec<u8>,
    event: usize,
}

#[derive(Default)]
struct State {
    config: BTreeMap<(PciAddress, u16), u32>,
    smn_index: BTreeMap<u8, u32>,
    smn: BTreeMap<(u8, u32), u32>,
    ficaa: BTreeMap<u8, u32>,
    df: BTreeMap<(u8, u32), u32>,
    fch: BTreeMap<(Space, u32), u8>,
    dxio: BTreeMap<u8, Dxio>,
    events: Vec<Event>,
    clock: u64,
}

/// A host model of the dies with root buses `buses`; `arguments` is the
/// SoC's SMU argument block, `dxio` its DXIO service message, `replies`
/// fixed first reply words of SMU messages.
pub struct Hw {
    buses: Vec<u8>,
    arguments: u32,
    dxio: u32,
    replies: &'static [(u32, u32)],
    state: RefCell<State>,
}
impl Hw {
    pub fn new(buses: &[u8], arguments: u32, dxio: u32, replies: &'static [(u32, u32)]) -> Self {
        let hw = Self {
            buses: buses.into(),
            arguments,
            dxio,
            replies,
            state: RefCell::default(),
        };
        for die in 0..buses.len() as u8 {
            hw.set_smn(die, RESPONSE, 1);
        }
        hw
    }
    pub fn set_config(&self, address: PciAddress, offset: u16, value: u32) {
        self.state
            .borrow_mut()
            .config
            .insert((address, offset), value);
    }
    /// Preset `bytes` bytes of an FCH register space, without a log entry.
    pub fn set_fch(&self, space: Space, address: u32, bytes: u8, value: u32) {
        let mut state = self.state.borrow_mut();
        for i in 0..u32::from(bytes) {
            state
                .fch
                .insert((space, address + i), (value >> (8 * i)) as u8);
        }
    }
    pub fn set_smn(&self, die: u8, address: impl Into<u32>, value: u32) {
        self.state
            .borrow_mut()
            .smn
            .insert((die, address.into()), value);
    }
    pub fn smn(&self, die: u8, address: impl Into<u32>) -> u32 {
        let address = address.into();
        let state = self.state.borrow();
        state
            .smn
            .get(&(die, address))
            .copied()
            .unwrap_or_else(|| noise(&[2, die.into(), address.into()]))
    }
    /// The clock: delays are logged, time moves a microsecond per read.
    pub fn clock(&self) -> Time<'_> {
        Time(self)
    }
    /// (writes and delays, order-sensitive digest).
    pub fn fingerprint(&self) -> (usize, u64) {
        let state = self.state.borrow();
        let hash = state
            .events
            .iter()
            .flat_map(|e| e.words())
            .fold(0xcbf2_9ce4_8422_2325u64, |h, w| {
                (h ^ w).wrapping_mul(0x0000_0100_0000_01b3)
            });
        (state.events.len(), hash)
    }
    /// A digest of the final register state, independent of the order of
    /// the writes.
    pub fn state(&self) -> u64 {
        let state = self.state.borrow();
        let config = state.config.iter().map(|(&(a, o), &v)| {
            [
                1,
                u64::from(a.bus()) << 16 | u64::from(a.device()) << 8 | u64::from(a.function()),
                o.into(),
                v.into(),
            ]
        });
        let smn = state
            .smn
            .iter()
            .map(|(&(d, a), &v)| [2, d.into(), a.into(), v.into()]);
        let df = state
            .df
            .iter()
            .map(|(&(n, f), &v)| [3, n.into(), f.into(), v.into()]);
        let fch = state
            .fch
            .iter()
            .map(|(&(s, a), &v)| [4, s as u64, a.into(), v.into()]);
        config
            .chain(smn)
            .chain(df)
            .chain(fch)
            .flatten()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, w| {
                (h ^ w).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }
    fn die_of(&self, bus: u8) -> Option<u8> {
        self.buses.iter().position(|&b| b == bus).map(|d| d as u8)
    }

    fn reply(&self, die: u8, message: u32, arguments: [u32; 6]) -> [u32; 6] {
        if message == self.dxio {
            return self.dxio(die, arguments);
        }
        let mut reply: [u32; 6] = core::array::from_fn(|i| {
            noise(&[5, die.into(), message.into(), arguments[0].into(), i as u64])
        });
        if let Some(&(_, first)) = self.replies.iter().find(|&&(m, _)| m == message) {
            reply[0] = first;
        }
        reply
    }

    /// The DXIO firmware: it takes the engine configuration with LoadData
    /// type 3, numbers the PCIe engines per core (the last one dropped) and
    /// returns them with EngineConfig.
    fn dxio(&self, die: u8, arguments: [u32; 6]) -> [u32; 6] {
        let mut state = self.state.borrow_mut();
        let dxio = state.dxio.entry(die).or_default();
        let ok = [3, 0, 0, 0, 0, 0];
        match arguments[0] {
            0x23 if arguments[5] == 3 => {
                let length = arguments[3] as usize * 4;
                let mut config = std::vec![0; length];
                // SAFETY: the driver's DMA buffer below 4 GiB.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        arguments[2] as usize as *const u8,
                        config.as_mut_ptr(),
                        length,
                    )
                };
                let count = usize::from(u16::from_le_bytes([config[12], config[13]]));
                let mut ports = [0u8; 2];
                let mut last = None;
                for i in 0..count {
                    let engine = &mut config[16 + 40 * i..16 + 40 * (i + 1)];
                    if engine[0] != 1 {
                        continue;
                    }
                    let core = usize::from(engine[2] >= 20);
                    engine[0x19] = ports[core];
                    ports[core] += 1;
                    last = Some(16 + 40 * i);
                }
                if let Some(engine) = last {
                    config[engine + 0x10] &= !1;
                }
                dxio.config = config;
                ok
            }
            0x307 => {
                dxio.event = 0;
                ok
            }
            9 => {
                let event = DXIO_EVENTS[dxio.event.min(DXIO_EVENTS.len() - 1)];
                dxio.event += 1;
                [event[0], event[1], event[2], 0, 0, 0]
            }
            0x14 => {
                let length = (arguments[3] as usize * 4).min(dxio.config.len());
                // SAFETY: as above.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        dxio.config.as_ptr(),
                        arguments[2] as usize as *mut u8,
                        length,
                    )
                };
                ok
            }
            _ => ok,
        }
    }

    fn smn_write(&self, die: u8, address: u32, value: u32) {
        self.set_smn(die, address, value);
        if address != MESSAGE {
            return;
        }
        let arguments = core::array::from_fn(|i| self.smn(die, self.arguments + 4 * i as u32));
        for (i, word) in self.reply(die, value, arguments).into_iter().enumerate() {
            self.set_smn(die, self.arguments + 4 * i as u32, word);
        }
        self.set_smn(die, RESPONSE, 1);
    }
}

impl ConfigRegionAccess for Hw {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        let (bus, device, function) = (address.bus(), address.device(), address.function());
        if let (Some(die), 0, 0) = (self.die_of(bus), device, function) {
            let index = match offset {
                SMN_DATA => self.state.borrow().smn_index[&bus],
                _ => NB_CONFIG + u32::from(offset),
            };
            return self.smn(die, index);
        }
        if (bus, function, offset) == (0, 4, FICAD) && (0x18..0x20).contains(&device) {
            let node = device - 0x18;
            let state = self.state.borrow();
            let ficaa = state.ficaa[&node];
            return state
                .df
                .get(&(node, ficaa))
                .copied()
                .unwrap_or_else(|| noise(&[3, node.into(), ficaa.into()]));
        }
        let state = self.state.borrow();
        state
            .config
            .get(&(address, offset))
            .copied()
            .unwrap_or_else(|| {
                noise(&[1, bus.into(), device.into(), function.into(), offset.into()])
            })
    }
    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        let (bus, device, function) = (address.bus(), address.device(), address.function());
        self.state
            .borrow_mut()
            .events
            .push(Event::Config(address, offset, value));
        if let (Some(die), 0, 0) = (self.die_of(bus), device, function) {
            match offset {
                SMN_INDEX => {
                    self.state.borrow_mut().smn_index.insert(bus, value);
                    return;
                }
                SMN_DATA => {
                    let index = self.state.borrow().smn_index[&bus];
                    self.smn_write(die, index, value);
                    return;
                }
                _ => {
                    self.set_smn(die, NB_CONFIG + u32::from(offset), value);
                    return;
                }
            }
        }
        if (bus, function) == (0, 4) && (0x18..0x20).contains(&device) {
            let node = device - 0x18;
            let mut state = self.state.borrow_mut();
            match offset {
                FICAA => {
                    state.ficaa.insert(node, value);
                    return;
                }
                FICAD => {
                    let ficaa = state.ficaa[&node];
                    state.df.insert((node, ficaa), value);
                    return;
                }
                _ => {}
            }
        }
        self.set_config(address, offset, value);
    }
}

impl Bus for Hw {
    fn read(&self, space: Space, address: u32, bytes: u8) -> u32 {
        let state = self.state.borrow();
        (0..u32::from(bytes)).rev().fold(0, |value, i| {
            let byte = state
                .fch
                .get(&(space, address + i))
                .copied()
                .unwrap_or_else(|| noise(&[4, space as u64, (address + i).into()]) as u8);
            (value << 8) | u32::from(byte)
        })
    }
    fn write(&self, space: Space, address: u32, bytes: u8, value: u32) {
        let mut state = self.state.borrow_mut();
        state.events.push(Event::Fch(space, address, bytes, value));
        for i in 0..u32::from(bytes) {
            state
                .fch
                .insert((space, address + i), (value >> (8 * i)) as u8);
        }
    }
}

pub struct Time<'a>(&'a Hw);
impl DelayNs for Time<'_> {
    fn delay_ns(&mut self, ns: u32) {
        self.delay_us(ns.div_ceil(1000))
    }
    fn delay_us(&mut self, us: u32) {
        self.0.state.borrow_mut().events.push(Event::Delay(us));
    }
}
impl Clock for Time<'_> {
    fn now_us(&mut self) -> u64 {
        let mut state = self.0.state.borrow_mut();
        state.clock += 1;
        state.clock
    }
}

/// `value` at `address` below 4 GiB, where the DXIO firmware can reach it.
/// A fixed address keeps the DMA addresses in the log stable.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub fn low<T>(address: usize, value: T) -> &'static mut T {
    let size = size_of::<T>().next_multiple_of(4096);
    // SAFETY: a fresh private mapping, never unmapped.
    unsafe {
        let pointer = libc::mmap(
            address as *mut libc::c_void,
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
            -1,
            0,
        );
        assert_eq!(pointer as usize, address);
        let pointer = pointer.cast::<T>();
        pointer.write(value);
        &mut *pointer
    }
}
