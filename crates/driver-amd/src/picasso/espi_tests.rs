// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::espi::*;
use std::{collections::BTreeMap, vec::Vec};
struct Timer(u64);
impl Clock for Timer {
    fn now_us(&mut self) -> u64 {
        self.0
    }
    fn pause_us(&mut self, us: u32) {
        self.0 += u64::from(us);
    }
}
struct Controller {
    regs: BTreeMap<u16, u32>,
    slave: BTreeMap<u16, u32>,
    writes: Vec<(u16, u32, u8)>,
    frames: Vec<[u32; 4]>,
    fail: Option<usize>,
}
impl Controller {
    fn new() -> Self {
        Self {
            regs: [(0x2c, 4 << 13)].into(),
            slave: [(8, 0x03020003), (0x20, 9 << 8), (0x10, 0x4000)].into(),
            writes: Vec::new(),
            frames: Vec::new(),
            fail: None,
        }
    }
}
impl Registers for Controller {
    fn read32(&mut self, offset: u16) -> u32 {
        self.regs.get(&offset).copied().unwrap_or(0)
    }
    fn write32(&mut self, offset: u16, value: u32) {
        self.writes.push((offset, value, 32));
        if offset == 0x70 {
            let old = self.read32(offset);
            self.regs.insert(offset, old & !value);
            return;
        }
        self.regs.insert(offset, value);
        if offset == 0 {
            let data = self.read32(4);
            let payload = self.read32(0xc);
            let header2 = self.read32(8);
            self.frames.push([value, data, header2, payload]);
            let address = ((value >> 8 & 255) << 8 | (value >> 16 & 255)) as u16;
            match value & 7 {
                1 => {
                    self.regs
                        .insert(4, self.slave.get(&address).copied().unwrap_or(0));
                }
                0 => {
                    self.slave.insert(
                        address,
                        if address != 8 && data & 1 != 0 {
                            data | 2
                        } else {
                            data
                        },
                    );
                }
                _ => (),
            }
            self.regs.insert(0, value & !8);
            self.regs.insert(
                0x70,
                if self.fail == Some(self.frames.len()) {
                    (1 << 28) | 4
                } else if value & 7 == 2 {
                    (1 << 28) | 16
                } else {
                    1 << 28
                },
            );
        }
    }
    fn read16(&mut self, offset: u16) -> u16 {
        self.read32(offset) as u16
    }
    fn write16(&mut self, offset: u16, value: u16) {
        self.writes.push((offset, u32::from(value), 16));
        self.regs.insert(offset, u32::from(value));
    }
    fn write8(&mut self, offset: u16, value: u8) {
        self.writes.push((offset, u32::from(value), 8));
        self.regs.insert(offset, u32::from(value));
    }
}
fn config() -> Config {
    Config {
        io: [
            Some(IoRange {
                base: 0x62,
                size: 5,
            }),
            Some(IoRange {
                base: 0x800,
                size: 256,
            }),
            Some(IoRange {
                base: 0x900,
                size: 255,
            }),
            Some(IoRange {
                base: 0x200,
                size: 8,
            }),
        ],
        port80: true,
        keyboard: true,
        superio: false,
        mode: Mode::Quad,
        frequency: Frequency::Mhz33,
        alert: Alert::PushPull,
        crc: true,
        peripheral: true,
        virtual_wire: true,
        oob: false,
        flash: false,
        subtractive: false,
        irq_polarity: 0,
    }
}
#[test]
fn dalboz_connection_channels_pltrst_and_exact_decode_widths() {
    let mut controller = Controller::new();
    assert_eq!(
        initialize(&mut controller, &mut Timer(0), &config()).unwrap(),
        0xe200000c
    );
    assert_eq!(
        controller.frames.as_slice(),
        &[
            [0x0a, 0, 0, 0],
            [0x080009, 0, 0, 0],
            [0x080008, 0x98200000, 0, 0],
            [0x200009, 0, 0, 0],
            [0x200008, 0x40001, 0, 0],
            [0x200009, 0, 0, 0],
            [0x0d, 0, 0, 0x2003],
            [0x0d, 0, 0, 0x2203],
            [0x100009, 0, 0, 0],
            [0x100008, 0x4005, 0, 0],
            [0x100009, 0, 0, 0],
        ]
    );
    assert_eq!(controller.regs[&0x40], 0xf06);
    assert_eq!(
        [0x44, 0x46, 0x48, 0x4a].map(|offset| controller.regs[&offset]),
        [0x62, 0x800, 0x900, 0x200]
    );
    assert_eq!(
        [0x4c, 0x4d, 0x4e, 0x4f].map(|offset| controller.regs[&offset]),
        [4, 255, 254, 7]
    );
    for &(offset, _, width) in &controller.writes {
        if (0x44..=0x4a).contains(&offset) {
            assert_eq!(width, 16);
        }
        if (0x4c..=0x4f).contains(&offset) {
            assert_eq!(width, 8);
        }
    }
    let polarity = controller
        .writes
        .iter()
        .position(|&(offset, _, _)| offset == 0xac)
        .unwrap();
    let enable = controller
        .writes
        .iter()
        .position(|&(offset, value, _)| offset == 0x68 && value & 4 != 0)
        .unwrap();
    assert!(polarity < enable);
    for (i, &(offset, _, _)) in controller.writes.iter().enumerate() {
        if offset == 0 {
            assert_eq!(
                [
                    controller.writes[i - 3].0,
                    controller.writes[i - 2].0,
                    controller.writes[i - 1].0
                ],
                [4, 8, 0xc]
            );
        }
    }
}
#[test]
fn malformed_ranges_preflight_and_command_error_stops_connection() {
    let mut invalid = config();
    invalid.io[0] = Some(IoRange {
        base: 0xfffe,
        size: 3,
    });
    let mut controller = Controller::new();
    assert_eq!(
        initialize(&mut controller, &mut Timer(0), &invalid),
        Err(Error::Window)
    );
    assert!(controller.writes.is_empty());
    let mut split = config();
    split.io = [
        Some(IoRange {
            base: 0x800,
            size: 512,
        }),
        None,
        None,
        None,
    ];
    assert_eq!(
        windows(&split).unwrap().as_slice(),
        &[
            IoRange {
                base: 0x800,
                size: 256
            },
            IoRange {
                base: 0x900,
                size: 256
            }
        ]
    );
    controller.fail = Some(3);
    assert_eq!(
        initialize(&mut controller, &mut Timer(0), &config()),
        Err(Error::Status((1 << 28) | 4))
    );
    assert_eq!(controller.frames.len(), 3);
    assert!(
        !controller
            .writes
            .iter()
            .any(|&(offset, _, _)| offset == 0xac)
    );
}
