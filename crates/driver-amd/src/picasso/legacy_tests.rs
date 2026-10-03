// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::legacy::*;
use std::{collections::BTreeMap, vec::Vec};
struct Mmio {
    data: [u32; 7],
    writes: Vec<(Register, u32)>,
}
impl Mmio {
    fn new() -> Self {
        Self {
            data: [0x80001003, 0xff, 0xff, 0xff, 0xff, 0x20, 0x80],
            writes: Vec::new(),
        }
    }
}
impl Registers for Mmio {
    fn read(&mut self, register: Register) -> u32 {
        self.data[register as usize]
    }
    fn write(&mut self, register: Register, value: u32) {
        self.data[register as usize] = value;
        self.writes.push((register, value));
    }
    fn status(&mut self, _: Status) -> u32 {
        0x400
    }
}
struct Ports {
    index: BTreeMap<u16, u8>,
    data: BTreeMap<(u16, u8), u8>,
    writes: Vec<(u16, u8)>,
    wake: u8,
}
impl Ports {
    fn new() -> Self {
        Self {
            index: BTreeMap::new(),
            data: [
                ((0xcd6, 0x0a), 0x80),
                ((0xcd6, 0xc0), 0xa0),
                ((0x70, 0xb), 0xff),
                ((0x70, 0xa), 0xff),
                ((0xc00, 8), 0xff),
                ((0xc00, 9), 0xff),
            ]
            .into(),
            writes: Vec::new(),
            wake: 0x45,
        }
    }
}
impl Io for Ports {
    fn read8(&mut self, port: u16) -> u8 {
        if port == 0x401 {
            self.wake
        } else {
            let index_port = port - 1;
            self.index
                .get(&index_port)
                .and_then(|index| self.data.get(&(index_port, *index)))
                .copied()
                .unwrap_or(0)
        }
    }
    fn write8(&mut self, port: u16, value: u8) {
        self.writes.push((port, value));
        match port {
            0xcd6 | 0x70 | 0xc00 => {
                self.index.insert(port, value);
            }
            0xcd7 | 0x71 | 0xc01 => {
                self.data.insert((port - 1, self.index[&(port - 1)]), value);
            }
            0x401 => self.wake &= !value, // ACPI status is W1C, not ordinary RAM.
            _ => (),
        }
    }
}
#[test]
fn legacy_preserves_control_bits_and_uses_indexed_byte_w1c_irq_transactions() {
    let mut registers = Mmio::new();
    let mut ports = Ports::new();
    disable_rom_sharing(&mut registers);
    assert_eq!(registers.data[Register::LpcControl as usize], 0xe7);
    initialize(&mut registers, &mut ports, true, false);
    lpc_init(&mut registers);
    assert_eq!(
        registers.data[Register::EspiInterruptMask as usize],
        0x80000001
    );
    assert_eq!(registers.data[Register::SdControl as usize], 0xbf);
    assert_eq!(registers.data[Register::SdEnable as usize], 0xfe);
    assert_eq!(registers.data[Register::SdPower as usize], 0xf7);
    assert_eq!(registers.data[Register::LpcA4 as usize], 0x21);
    assert_eq!(registers.data[Register::LpcClock as usize], 0x80000080);
    assert_eq!(ports.data[&(0xcd6, 0x0a)], 0x98);
    assert_eq!(ports.data[&(0xcd6, 0xc0)], 0xa2);
    assert_eq!(ports.data[&(0x70, 0xb)], 0xfe);
    assert_eq!(ports.data[&(0x70, 0xa)], 0xef);
    assert_eq!(ports.data[&(0xc00, 8)], 0xff);
    assert_eq!(ports.data[&(0xc00, 9)], 0x99);
    assert!(ports.writes.contains(&(0xb14, 0)));
    assert!(ports.writes.contains(&(0xb2a, 0x2f)));
    assert_eq!(
        ports
            .writes
            .iter()
            .filter(|&&(port, _)| port == 0x401)
            .copied()
            .collect::<Vec<_>>(),
        [(0x401, 0x40), (0x401, 0x40)]
    );
    assert_eq!(ports.wake, 5);
    let expected = [
        (0x80, 16),
        (0x81, 17),
        (0x82, 18),
        (0x83, 19),
        (0x97, 16),
        (0x9a, 16),
        (0xc3, 5),
        (0xe2, 7),
        (0xf6, 3),
        (0xf7, 4),
        (0xf8, 15),
        (0xf9, 5),
        (0xf0, 10),
        (0xf1, 11),
        (0xf2, 14),
        (0xf3, 6),
        (0xf4, 14),
        (0xf5, 15),
    ];
    let irq_writes = ports
        .writes
        .windows(2)
        .filter_map(|pair| {
            let [(index_port, index), (data_port, irq)] = pair else {
                unreachable!()
            };
            (*index_port == 0xc00 && *data_port == 0xc01 && index & 0x80 != 0)
                .then_some((*index, *irq))
        })
        .collect::<Vec<_>>();
    assert_eq!(irq_writes, expected);
}
#[test]
fn retained_legacy_sd_and_lpc_only_policy_do_not_power_off_or_mask_espi() {
    let mut registers = Mmio::new();
    let mut ports = Ports::new();
    initialize(&mut registers, &mut ports, false, true);
    assert!(registers.writes.is_empty());
}
