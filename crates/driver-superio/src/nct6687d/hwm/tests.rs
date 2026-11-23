// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use std::{cell::RefCell, rc::Rc, vec::Vec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Page(u8),
    Index(u8),
    Read(u16, u8),
    Write(u16, u8),
    Delay(u32),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Fault {
    #[default]
    None,
    Exit,
    MissingPhase,
    Invalid,
    NoLock,
}
struct State {
    regs: [u8; 65536],
    page: u8,
    index: Option<u8>,
    events: Vec<Event>,
    fault: Fault,
    delayed_ack: u32,
    pending: Option<u8>,
    done_count: usize,
    fault_on_done: Option<usize>,
}
struct Endpoint {
    kind: u8,
    state: Rc<RefCell<State>>,
}
impl Readable for Endpoint {
    type T = u8;
    type R = ();
    fn get(&self) -> u8 {
        assert_eq!(self.kind, 2);
        let mut s = self.state.borrow_mut();
        assert_ne!(s.page, 0xff, "data read outside selected page");
        let addr = u16::from_be_bytes([s.page, s.index.expect("selected index")]);
        let value = s.regs[addr as usize];
        s.events.push(Event::Read(addr, value));
        value
    }
}
impl Writeable for Endpoint {
    type T = u8;
    type R = ();
    fn set(&self, value: u8) {
        let mut s = self.state.borrow_mut();
        match self.kind {
            0 => {
                assert!(
                    s.page == 0xff || value == 0xff,
                    "page must close before switching"
                );
                s.page = value;
                s.index = None;
                s.events.push(Event::Page(value));
            }
            1 => {
                assert_ne!(s.page, 0xff);
                assert!(
                    s.index.is_none(),
                    "ordinary HWM transaction must not send index ff"
                );
                s.index = Some(value);
                s.events.push(Event::Index(value));
            }
            2 => {
                assert_ne!(s.page, 0xff);
                let addr = u16::from_be_bytes([s.page, s.index.unwrap()]);
                if addr != FAN_CFG_CTRL {
                    assert_eq!(
                        s.regs[FAN_ENGINE_STS as usize] & 0x48,
                        8,
                        "configuration write outside unlocked phase"
                    );
                }
                s.events.push(Event::Write(addr, value));
                s.regs[addr as usize] = value;
                if addr == FAN_CFG_CTRL {
                    if value == 0x40 {
                        s.done_count += 1;
                    }
                    let fault = if s.fault_on_done.is_some_and(|n| n != s.done_count) {
                        Fault::None
                    } else {
                        s.fault
                    };
                    let status = if value == 0x80 {
                        match fault {
                            Fault::MissingPhase => 0,
                            _ => 8,
                        }
                    } else {
                        match fault {
                            Fault::Invalid => 0x70,
                            Fault::NoLock => 0x20,
                            _ => 0x60,
                        }
                    };
                    if s.delayed_ack != 0 {
                        s.pending = Some(status);
                    } else {
                        s.regs[FAN_ENGINE_STS as usize] = status;
                    }
                }
            }
            _ => unreachable!(),
        }
    }
}
struct Delay(Rc<RefCell<State>>);
impl DelayNs for Delay {
    fn delay_ns(&mut self, ns: u32) {
        let mut s = self.0.borrow_mut();
        s.events.push(Event::Delay(ns));
        if s.delayed_ack > 0 {
            s.delayed_ack -= 1;
            if s.delayed_ack == 0
                && let Some(status) = s.pending.take()
            {
                s.regs[FAN_ENGINE_STS as usize] = status;
            }
        }
    }
}
fn fixture(fault: Fault) -> (Ec<Endpoint, Endpoint, Endpoint>, Delay, Rc<RefCell<State>>) {
    let mut regs = [0xa5; 65536];
    regs[FAN_ENGINE_STS as usize] = if fault == Fault::Exit { 0x48 } else { 0x60 };
    regs[FAN_CFG_CTRL as usize] = 0;
    regs[0xcf9] = 0x40;
    let state = Rc::new(RefCell::new(State {
        regs,
        page: 0xff,
        index: None,
        events: Vec::new(),
        fault,
        delayed_ack: 0,
        pending: None,
        done_count: 0,
        fault_on_done: None,
    }));
    let endpoint = |kind| Endpoint {
        kind,
        state: state.clone(),
    };
    (
        Ec::new(endpoint(0), endpoint(1), endpoint(2)),
        Delay(state.clone()),
        state,
    )
}
fn smart() -> Fan {
    Fan {
        mode: Mode::SmartFan(SmartFan {
            sources: [1, 2, 4, 8],
            temperatures: [30, 40, 50, 60, 70, 80, 90],
            speeds: [1000, 1500, 2000, 2500, 3000, 3500, 4000],
            hysteresis: 5,
            cutoff: 20,
            cutoff_delay: 7,
            step_up: 3,
            step_down: 9,
        }),
        unit: Unit::Rpm,
        input: 2,
        output: 3,
        ..Fan::IGNORED
    }
}
fn scenario(name: &str) -> Config {
    let mut c = Config::SOURCE_ZERO;
    match name {
        "zero" => {}
        "smart" | "pwm" | "full" => {
            c.fans[0] = smart();
        }
        "thermal" => {
            c.fans[0].mode = Mode::ThermalCruise(ThermalCruise {
                keep_min: true,
                target: 60,
                tolerance: 4,
                initial: 120,
                stop: 30,
                stop_time: 8,
                step_up: 2,
                step_down: 7,
            });
        }
        "speed" => {
            c.fans[0].mode = Mode::SpeedCruise(SpeedCruise {
                target: 0x1234,
                tolerance: 0x123,
                step_up: 2,
                step_down: 7,
            });
        }
        "channel9" => {
            c.fans[8] = smart();
            c.fans[8].weight = 3;
        }
        "channel10" => {
            c.fans[9] = smart();
            c.fans[9].weight = 4;
        }
        "peci" => {
            c.sensors[0] = Sensor(0x20);
            c.sensors[1] = Sensor(0x27);
            c.peci_speed = 2;
        }
        "smbus" => {
            c.sensors[2] = Sensor(0x30);
            c.smbus_sensor = Some(SmbusSensor {
                index: 2,
                one_byte: true,
                port: 2,
                baud: 3,
                address: 0x90,
                command: 0x12,
            });
        }
        _ => panic!("unknown fixture"),
    }
    if name == "pwm" {
        c.fans[0].unit = Unit::Pwm;
        if let Mode::SmartFan(ref mut s) = c.fans[0].mode {
            s.speeds = [10, 25, 40, 55, 70, 85, 100];
        }
    }
    if name == "full" {
        c.fans[1] = smart();
        for (i, f) in c.fans.iter_mut().enumerate().take(2) {
            f.weight = i as u8 + 3;
            f.critical_temp = 100;
            f.critical_tolerance = 4;
            f.temp_error_duty = 255;
            f.dts2 = true;
            f.dts_upper_bound = true;
            f.smart_tracking = true;
            f.fast_tracking = true;
            f.markup_tracking = true;
            f.ambient_floor = true;
            f.ambient_rpm = [0, 1200, 4500];
            f.startup_percent = 40;
            f.manual_offset = 3;
            f.min_percent = 10;
        }
        c.fan_default_percent = 75;
        c.dts2 = Dts2 {
            target_margin: -10,
            tolerance: 3,
            tcontrol: -20,
            tcontrol_offset: -5,
            step_speed: 0x1234,
            min_speed: 0x5678,
            delay_seconds: 9,
            divisor: 7,
        };
        c.ambient_temperatures = [20, 45];
        c.tracking = Tracking {
            tolerance: [1, 2, 3],
            boundary_low: 0x1234,
            boundary_high: 0x5678,
            step_up: 9,
            step_down: 4,
            temp_boundary: 50,
            weight_up: [1, 2, 3],
            weight_down: [4, 5, 6],
            duty_step: [3, 4, 5],
            ambient_boundary: 30,
            markup_weight: 7,
        };
    }
    c
}
fn writes(s: &State) -> Vec<(u16, u8)> {
    s.events
        .iter()
        .filter_map(|e| {
            if let Event::Write(a, v) = e {
                Some((*a, *v))
            } else {
                None
            }
        })
        .collect()
}
fn assert_transactions(s: &State) {
    let mut i = 0;
    while i < s.events.len() {
        if let Event::Delay(_) = s.events[i] {
            i += 1;
            continue;
        }
        assert_eq!(s.events[i], Event::Page(0xff));
        i += 1;
        assert!(matches!(s.events[i],Event::Page(p) if p!=0xff));
        i += 1;
        assert!(matches!(s.events[i], Event::Index(_)));
        i += 1;
        assert!(matches!(s.events[i], Event::Read(..) | Event::Write(..)));
        i += 1;
        if matches!(s.events[i], Event::Write(..)) {
            i += 1;
        }
        assert_eq!(s.events[i], Event::Page(0xff));
        i += 1;
    }
    assert_eq!(s.page, 0xff);
}

#[test]
fn nct6687d_hwm_scenarios_program_valid_ec_page_transactions() {
    for name in [
        "zero",
        "smart",
        "pwm",
        "full",
        "thermal",
        "speed",
        "channel9",
        "channel10",
        "smbus",
        "peci",
    ] {
        let (mut ec, mut delay, state) = fixture(Fault::None);
        initialize(&mut ec, &mut delay, &scenario(name)).unwrap();
        assert_transactions(&state.borrow());
        assert!(!writes(&state.borrow()).is_empty(), "{name}");
    }
}

#[test]
fn nct6687d_hwm_configuration_errors_not_open_and_missing_lock_propagate() {
    for (fault, expected) in [
        (
            Fault::Invalid,
            Error::ConfigurationRejected {
                status: 0x70,
                code: 0x40,
            },
        ),
        (
            Fault::NoLock,
            Error::ConfigurationNotLocked { status: 0x20 },
        ),
        (
            Fault::MissingPhase,
            Error::ConfigurationNotOpen { status: 0 },
        ),
    ] {
        let (mut ec, mut delay, state) = fixture(fault);
        assert_eq!(
            initialize(&mut ec, &mut delay, &scenario("smart")),
            Err(expected)
        );
        assert!(!writes(&state.borrow()).iter().any(|(a, _)| *a == 0x9d0));
        assert_transactions(&state.borrow());
    }
}

#[test]
fn nct6687d_hwm_firmware_info_reads_actual_source_words_and_bytes() {
    let (mut ec, _, state) = fixture(Fault::None);
    for i in 0..0x20 {
        state.borrow_mut().regs[0x600 + i] = i as u8;
    }
    let info = read_info(&mut ec);
    assert_eq!(info.chip_id, 0x0100);
    assert_eq!(info.customer_id, 0x0302);
    assert_eq!(info.build, [6, 5, 4, 7]);
    assert_eq!(info.firmware, [8, 9]);
    assert_eq!(info.profile, 10);
    assert_eq!(info.rom, [12, 13, 14, 15]);
    assert_eq!(info.isp_build, [20, 19, 18, 21]);
    assert_eq!(info.isp, [22, 23]);
    assert_eq!(info.oem, [24, 25, 26, 27, 28, 29, 30, 31]);
    assert!(writes(&state.borrow()).is_empty());
    assert_transactions(&state.borrow());
}
