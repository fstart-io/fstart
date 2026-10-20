// SPDX-License-Identifier: MIT OR Apache-2.0
//! Whole-sequence write logs of the Family 17h silicon init: a host model
//! of the dies (PCI config space, SMN behind D0F0 0xb8/0xbc, the DF FICAA
//! pair, an SMU with a scripted DXIO firmware, the FCH register spaces)
//! logs every write and delay in order. Unset registers read as noise, so
//! a read-modify-write and a plain write differ. The fingerprints pin the
//! sequences the hardware was tested with.
extern crate std;
use crate::{
    fam17h::tsc::{Clock, DelayNs},
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
fn low<T>(address: usize, value: T) -> &'static mut T {
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

#[cfg(all(feature = "zeppelin", target_arch = "x86_64", target_os = "linux"))]
mod zeppelin {
    use super::*;
    use crate::{
        fam17h::topology,
        fch::{Acpimmio, Fch},
        zeppelin::{
            Die, Package, Revision,
            dxio::{self, Descriptor, SataChannel},
            fabric,
            fch::{self, Devices},
            iommu, nbio, psp, smu,
        },
    };

    const fn pcie(start: u8, end: u8, device: u8, function: u8) -> Descriptor {
        Descriptor {
            turn_off_unused_lanes: true,
            ..Descriptor::pcie(start, end, device, function)
        }
    }
    const DIE0: &[Descriptor] = &[
        Descriptor::sata(0, 3, SataChannel::Other),
        pcie(4, 7, 1, 2),
        pcie(8, 11, 1, 3),
        pcie(12, 13, 1, 4),
        pcie(14, 14, 1, 5),
        pcie(15, 15, 1, 1),
        pcie(31, 24, 3, 1),
        pcie(23, 16, 3, 3),
    ];
    const DIE1: &[Descriptor] = &[pcie(0, 7, 1, 1), pcie(16, 31, 3, 1)];
    const SUBSYSTEM_ID: u32 = 0x1000_1458;
    const DEVICES: [Devices; 2] = [
        Devices {
            xhci: true,
            sata: true,
        },
        Devices {
            xhci: true,
            sata: true,
        },
    ];

    fn sp4() -> Hw {
        let hw = Hw::new(
            &[0, 0x40],
            smu::MAILBOX.arguments.address(),
            smu::DXIO_SERVICE,
            &[(smu::GET_VERSION, 7)],
        );
        let df = |node: u8, function| PciAddress::new(0, 0, 0x18 + node, function);
        hw.set_config(df(0, 1), 0x204, 2);
        hw.set_config(df(0, 0), 0x84, 0);
        hw.set_config(df(1, 0), 0x84, 0x40);
        hw.set_smn(0, smu::THREAD_CONFIG, 0x013);
        hw.set_smn(1, smu::THREAD_CONFIG, 0x103);
        hw.set_smn(0, smu::THREAD_ENABLE, 1);
        hw.set_smn(1, smu::THREAD_ENABLE, 0);
        hw.set_smn(0, 0x13f0_0040_u32, 0x1964_000f);
        // Die 1's internal bridge 8.1 exists.
        hw.set_config(PciAddress::new(0, 0x40, 8, 1), 0, 0x1452_1022);
        hw
    }

    #[test]
    fn sp4_silicon_init_sequence() {
        let hw = sp4();
        let pci = &hw;
        let dies: Vec<Die<&Hw>> = unsafe { topology::dies(pci) }.collect();
        let mut delay = hw.clock();

        // Before the PCIe training
        let plan = fabric::plan(0xe000_0000, 0x4_2000_0000, 128 << 20, 2).unwrap();
        let dfs: Vec<_> = dies.iter().map(Die::df).collect();
        fabric::program(&plan, &dfs);
        let nonpci: Vec<u32> = dies
            .iter()
            .map(|die| fabric::nonpci_mmio_base(&dies[0].df(), die.index()).unwrap())
            .collect();
        assert_eq!(fabric::mmio_windows(&dies[0].df(), 1).len(), 2);
        for die in &dies {
            fabric::early_init(&die.df(), 2, Revision::B2);
        }
        let fch0 = Fch::new(pci, Acpimmio::DIE0);
        fch::reset_init(&fch0, Revision::B2);
        fch::sata::reset(&dies[0].smn(), &mut delay);
        fch::multi::reset(
            pci,
            &dies,
            Package::Sp4,
            |d| nonpci[usize::from(d)],
            &mut delay,
        );
        for die in &dies {
            nbio::early_init(die, Revision::B2);
            psp::init_bar(die, nonpci[usize::from(die.index())]);
        }
        smu::print_name(&dies[0], &mut delay);
        for die in &dies {
            nbio::tom_init(die, 0xe000_0000, Some(0x4_2000_0000));
        }

        // PCIe training and after
        let ioapics: Vec<u32> = dies
            .iter()
            .map(|die| {
                iommu::init_bar(die, nonpci[usize::from(die.index())]);
                nbio::ioapic_init(die, nonpci[usize::from(die.index())])
            })
            .collect();
        assert_eq!(ioapics[0], nonpci[0] + (7 << 20));
        assert_eq!(psp::bars(&dies), [nonpci[0], nonpci[1]]);
        let buffers = low(0x4000_0000, dxio::Buffers::new());
        let topology =
            unsafe { dxio::init(&dies, &[DIE0, DIE1], SUBSYSTEM_ID, buffers, &mut delay) }.unwrap();
        let trained = |die: usize| {
            topology[die]
                .ports
                .iter()
                .flatten()
                .filter(|p| p.trained)
                .count()
        };
        assert_eq!(topology[0].ports.iter().flatten().count(), 7);
        assert!(trained(0) > 0 && trained(0) < 7);
        assert_eq!(topology[1].ports.iter().flatten().count(), 2);
        fch::usb::init(&dies[0].smn(), 0, Package::Sp4, &mut delay);
        fch::multi::usb(&dies, &DEVICES, Package::Sp4, &mut delay);
        let buffers = smu::DmaBuffers {
            policy: 0x7000_0040,
            agm_log: [0x7000_1000, 0x7000_2000],
            perfmon: [0x7000_3000, 0x7000_4000],
        };
        unsafe { smu::init(&dies, &buffers, &mut delay) }.unwrap();
        smu::cac_weights(&dies[0], &mut delay).unwrap();
        assert_eq!(fabric::sockets(&dies[0].df()), 2);
        for die in &dies {
            fabric::route_vga(&die.df(), 0);
        }
        for die in &dies {
            nbio::pre_enumeration_init(die);
        }
        fch::env_init(&fch0, &dies[0].smn(), SUBSYSTEM_ID);
        fch::sata::init(&fch0, 0, &dies[0].smn(), SUBSYSTEM_ID, &mut delay);
        fch::multi::xgbe_init(&dies[0].smn());
        fch::multi::env(
            pci,
            &dies,
            &DEVICES,
            |d| nonpci[usize::from(d)],
            SUBSYSTEM_ID,
            &mut delay,
        );

        // MP
        let threads = smu::thread_count(&dies);
        assert_eq!(threads, 20);
        unsafe { smu::launch_aps(&dies, threads - 1, &mut delay) }.unwrap();
        smu::aps_ready(&dies, &mut delay).unwrap();
        smu::ccx_done(&dies, &mut delay).unwrap();

        // ReadyToBoot
        fch::multi::late(&dies, |die| {
            (die == 1).then(|| PciAddress::new(0, 0x40, 8, 2))
        });
        fch::late_init(&fch0);
        for die in &dies {
            fabric::disable_ext_cfg(&die.df());
        }
        assert_eq!(hw.fingerprint(), (16906, 0x9dd1_7783_e797_1b5e));
        assert_eq!(hw.state(), 0xbc50_f84e_0701_a314);
    }
}

#[cfg(all(feature = "picasso", target_arch = "x86_64", target_os = "linux"))]
mod picasso {
    use super::*;
    use crate::{
        fam17h::smu::{THREAD_CONFIG, THREAD_ENABLE},
        picasso::{
            dxio::{self, Aspm, ClockRequest, Descriptor, Options, PowerPolicy},
            fabric,
            nbio::{self},
            registers::{Df, Smn},
            smu,
            topology::{self, Downcore, Policy},
        },
    };

    const fn pcie(first: u8, last: u8, function: u8, clock: ClockRequest) -> Descriptor {
        let mut descriptor = Descriptor::pcie(first, last, 1, function);
        descriptor.aspm = Aspm::L1;
        descriptor.clock_request = clock;
        descriptor.turn_off_unused_lanes = true;
        descriptor
    }
    const DALBOZ: &[Descriptor] = &[
        pcie(4, 5, 7, ClockRequest::Pin2),
        pcie(0, 0, 2, ClockRequest::Pin0),
        pcie(1, 1, 3, ClockRequest::Pin1),
    ];

    #[test]
    fn picasso_silicon_init_sequence() {
        let hw = Hw::new(&[0], smu::ARGUMENTS.address(), smu::DXIO_SERVICE, &[]);
        hw.set_smn(0, THREAD_CONFIG, 0x13);
        hw.set_smn(0, THREAD_ENABLE, 1);
        hw.set_smn(0, crate::fam17h::nbio::SMU_BASE_ADDR_LO, 0);
        hw.set_smn(0, crate::fam17h::nbio::SMU_BASE_ADDR_HI, 0);
        hw.set_smn(0, crate::fam17h::nbio::PSP_BASE_ADDR_LO, 0);
        hw.set_smn(0, crate::fam17h::nbio::pcie::link_status(0), 0);
        // TOM2[40:32] bit 8 clear: below 1 TiB.
        hw.set_smn(0, 0x13b1_0068_u32, 0x1234_5600);
        let smn = unsafe { Smn::new(&hw) };
        let df = unsafe { Df::new(&hw) };
        let mut delay = hw.clock();

        fabric::early_init(&df);
        fabric::late_init(&df, true);
        nbio::early_init(&smn, true, false, &mut delay).unwrap();
        nbio::psp_bar(&smn).unwrap();
        assert!(nbio::psp_bar_installed(&smn));
        nbio::dram_routes(&smn, 0xd000_0000, Some(0x4_3000_0000)).unwrap();
        nbio::ioapic_decode(&smn);

        let policy = Policy {
            downcore: Downcore::Two,
            disable_smt: true,
        };
        assert_eq!(
            topology::apply(&smn, policy, &mut delay).unwrap(),
            topology::Effect::ResetRequired
        );
        let topology = topology::query(&smn, policy).unwrap();
        unsafe { topology::launch(&smn, topology, 3, &mut delay) }.unwrap();

        let options = Options {
            power: PowerPolicy::Performance,
            am4: false,
            silicon_id: 0x8000_0000,
            graphics: true,
            hda_enabled: false,
            acp_enabled: true,
        };
        let buffers = low(0x4800_0000, dxio::Buffers::new());
        let ports =
            unsafe { dxio::train(&smn, &hw, buffers, DALBOZ, options, &mut delay) }.unwrap();
        assert_eq!(
            ports
                .ports
                .iter()
                .filter(|p| p.descriptor.is_some())
                .count(),
            3
        );
        assert_eq!(ports.ports.iter().filter(|p| p.trained).count(), 2);
        assert_eq!(hw.fingerprint(), (3492, 0xbc80_eb5b_f435_f511));
        assert_eq!(hw.state(), 0xc446_ef2a_df4d_6ca4);
    }
}
