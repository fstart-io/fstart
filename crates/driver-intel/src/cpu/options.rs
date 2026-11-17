//! Optional legacy SpeedStep voltage policy, shared by AML and Penryn EMTTM.

use fstart_options::{Kind, Knob, Meta};

#[derive(Clone, Copy, Default)]
pub struct Options {
    /// Voltage reduction in 12.5 mV steps. Zero retains stock voltage.
    pub undervolt_steps: u8,
}

impl Options {
    /// Stock voltage: what a board offers before any override.
    pub const STOCK: Self = Self { undervolt_steps: 0 };

    /// Apply before AP startup and before emitting CPU ACPI tables.
    pub fn apply(self) {
        fstart_arch::x86::cpu::intel::speedstep::set_undervolt_steps(self.undervolt_steps);
    }
}

pub const UNDERVOLT: Knob<Options> = Knob::new(
    Meta {
        key: "cpu_undervolt",
        form: "Power",
        label: "CPU undervolt (12.5 mV steps)",
        help: "Lower CPU P-state voltage by 0 to 150 mV. May cause instability; zero uses stock voltage.",
        kind: Kind::Number {
            min: 0,
            max: 12,
            step: 1,
        },
    },
    |o: &Options| o.undervolt_steps.into(),
    |o: &mut Options, value| o.undervolt_steps = value as u8,
);
