//! Intel Core/Core 2 processor power-management AML helpers.
//! Shared SpeedStep probing/table generation lives in the CPU architecture
//! module so AML and Penryn EMTTM use the same states.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
#[cfg(any(feature = "gm45", feature = "gm965", feature = "i945"))]
use fstart_arch::x86::cpu::intel::speedstep::model_id;
use fstart_arch::x86::cpu::intel::speedstep::{PState as SpeedstepState, current_pstates, fsb_x3};

const SW_ANY: u32 = 0xfd;
#[cfg(any(feature = "gm45", feature = "gm965", feature = "i945"))]
const HW_ALL: u32 = 0xfe;

#[derive(Clone, Copy)]
struct CState {
    ctype: u8,
    latency: u16,
    power: u32,
    space_id: u8,
    bit_width: u8,
    bit_offset: u8,
    access_size: u8,
    address: u64,
}

/// Core 2 mobile MWAIT C1/C2/C3 and generic Intel SpeedStep devices.
#[cfg(any(feature = "gm45", feature = "gm965", feature = "i945"))]
pub fn cpu_devices_aml(logical_cpus: usize) -> Vec<u8> {
    const CSTATES: &[CState] = &[
        CState {
            ctype: 1,
            latency: 1,
            power: 1000,
            space_id: 0x7f,
            bit_width: 1,
            bit_offset: 2,
            access_size: 1,
            address: 0x00,
        },
        CState {
            ctype: 2,
            latency: 1,
            power: 500,
            space_id: 0x7f,
            bit_width: 1,
            bit_offset: 2,
            access_size: 1,
            address: 0x10,
        },
        CState {
            ctype: 3,
            latency: 17,
            power: 250,
            space_id: 0x7f,
            bit_width: 1,
            bit_offset: 2,
            access_size: 1,
            address: 0x20,
        },
    ];
    build_cpu_devices_aml(
        logical_cpus,
        Some(CSTATES),
        if model_id() == 0x1067 { HW_ALL } else { SW_ANY },
    )
}

#[cfg(feature = "pineview")]
pub(crate) fn pineview_cpu_devices_aml(logical_cpus: usize) -> Vec<u8> {
    build_cpu_devices_aml(logical_cpus, None, SW_ANY)
}

fn build_cpu_devices_aml(
    logical_cpus: usize,
    cstates: Option<&[CState]>,
    coordination: u32,
) -> Vec<u8> {
    assert!(
        logical_cpus <= 255,
        "CPU AML package exceeds 255 processors"
    );
    // SAFETY: these northbridges are paired with matched legacy Intel CPUs;
    // host namespace tests use synthetic limits instead of privileged reads.
    let states = unsafe { current_pstates() };
    let mut out = Vec::new();
    for cpu in 0..logical_cpus {
        let mut body = Vec::new();
        name_string(&mut body, "_HID", "ACPI0007");
        name_integer(&mut body, "_UID", cpu as u32);
        append_empty_pct(&mut body);
        append_psd(&mut body, 0, logical_cpus as u32, coordination);
        append_pss(&mut body, &states);
        if let Some(entries) = cstates {
            append_cst(&mut body, entries);
        }
        append_device(&mut out, cpu_name(cpu), &body);
    }
    append_processor_package(&mut out, logical_cpus);
    append_cnot_method(&mut out, logical_cpus);
    out
}

fn cpu_name(cpu: usize) -> [u8; 4] {
    [
        b'C',
        b'P',
        hex_digit(((cpu / 16) & 0xf) as u8),
        hex_digit((cpu & 0xf) as u8),
    ]
}
fn hex_digit(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'A' + n - 10 }
}
fn append_device(out: &mut Vec<u8>, name: [u8; 4], body: &[u8]) {
    let name = core::str::from_utf8(&name).expect("CPU AML names are ASCII");
    let device = fstart_acpi::aml_linker::device_vec(name, body)
        .expect("required CPU AML device exceeds length limit");
    out.extend_from_slice(&device);
}
fn append_processor_package(out: &mut Vec<u8>, logical_cpus: usize) {
    let mut elements = Vec::with_capacity(logical_cpus * 4);
    for cpu in 0..logical_cpus {
        elements.extend_from_slice(&cpu_name(cpu));
    }
    append_name(out, b"PPKG", |out| {
        append_package(out, logical_cpus as u8, &elements)
    });
}
fn append_cnot_method(out: &mut Vec<u8>, logical_cpus: usize) {
    let mut body = Vec::new();
    for cpu in 0..logical_cpus {
        body.push(0x86); // NotifyOp
        body.extend_from_slice(&cpu_name(cpu));
        body.push(0x68); // Arg0Op
    }
    out.push(0x14); // MethodOp
    let mut payload = Vec::new();
    payload.extend_from_slice(b"CNOT");
    payload.push(0x01); // one argument, NotSerialized
    payload.extend_from_slice(&body);
    let (length, width) = fstart_acpi::aml_linker::package_length(payload.len())
        .expect("required CPU AML package exceeds length limit");
    out.extend_from_slice(&length[..width]);
    out.extend_from_slice(&payload);
}
fn append_empty_pct(out: &mut Vec<u8>) {
    // Coreboot acpigen_write_empty_PCT(): two FFixedHW Register descriptors.
    out.extend_from_slice(&[
        0x08, 0x5f, 0x50, 0x43, 0x54, 0x12, 0x2c, 0x02, 0x11, 0x14, 0x0a, 0x11, 0x82, 0x0c, 0x00,
        0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x79, 0x00, 0x11,
        0x14, 0x0a, 0x11, 0x82, 0x0c, 0x00, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x79, 0x00,
    ]);
}
fn append_psd(out: &mut Vec<u8>, domain: u32, numprocs: u32, coordination: u32) {
    let mut inner = Vec::new();
    [5, 0, domain, coordination, numprocs]
        .into_iter()
        .for_each(|value| append_integer(&mut inner, value));
    let mut inner_package = Vec::new();
    append_package(&mut inner_package, 5, &inner);
    append_name(out, b"_PSD", |out| append_package(out, 1, &inner_package));
}
fn append_pss(out: &mut Vec<u8>, states: &[SpeedstepState]) {
    // SAFETY: same legacy Intel platform contract as current_pstates above.
    let fsb3 = unsafe { fsb_x3() }.unwrap_or(600);
    let mut entries = Vec::new();
    for (i, state) in states.iter().enumerate() {
        let freq = if state.is_turbo && i + 1 < states.len() {
            states[i + 1].double_ratio() * fsb3 / 6 + 1
        } else if state.is_slfm {
            state.double_ratio() * fsb3 / 12
        } else {
            state.double_ratio() * fsb3 / 6
        };
        let encoded = state.encoded();
        let mut values = Vec::new();
        [freq, state.power, 0, 0, encoded, encoded]
            .into_iter()
            .for_each(|value| append_integer(&mut values, value));
        append_package(&mut entries, 6, &values);
    }
    append_name(out, b"_PSS", |out| {
        append_package(
            out,
            u8::try_from(states.len()).expect("too many CPU P-states"),
            &entries,
        )
    });
    name_integer(out, "_PPC", 0);
}
fn append_cst(out: &mut Vec<u8>, entries: &[CState]) {
    let mut elements = Vec::new();
    append_integer(&mut elements, entries.len() as u32);
    for entry in entries {
        let mut descriptor = vec![
            0x82,
            0x0c,
            0x00,
            entry.space_id,
            entry.bit_width,
            entry.bit_offset,
            entry.access_size,
        ];
        descriptor.extend_from_slice(&entry.address.to_le_bytes());
        descriptor.extend_from_slice(&[0x79, 0x00]);
        let mut values = Vec::new();
        append_buffer(&mut values, &descriptor);
        append_integer(&mut values, u32::from(entry.ctype));
        append_integer(&mut values, u32::from(entry.latency));
        append_integer(&mut values, entry.power);
        append_package(&mut elements, 4, &values);
    }
    append_name(out, b"_CST", |out| {
        append_package(
            out,
            u8::try_from(entries.len() + 1).expect("too many CPU C-states"),
            &elements,
        )
    });
}
fn append_name(out: &mut Vec<u8>, name: &[u8; 4], value: impl FnOnce(&mut Vec<u8>)) {
    out.push(0x08);
    out.extend_from_slice(name);
    value(out);
}
fn append_integer(out: &mut Vec<u8>, value: u32) {
    match value {
        0 => out.push(0x00),
        1 => out.push(0x01),
        u32::MAX => out.push(0xff),
        2..=0xff => out.extend_from_slice(&[0x0a, value as u8]),
        0x100..=0xffff => {
            out.push(0x0b);
            out.extend_from_slice(&(value as u16).to_le_bytes());
        }
        _ => {
            out.push(0x0c);
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
}
fn append_package(out: &mut Vec<u8>, count: u8, elements: &[u8]) {
    out.push(0x12);
    let (length, width) = fstart_acpi::aml_linker::package_length(elements.len() + 1)
        .expect("required CPU AML package exceeds length limit");
    out.extend_from_slice(&length[..width]);
    out.push(count);
    out.extend_from_slice(elements);
}
fn append_buffer(out: &mut Vec<u8>, bytes: &[u8]) {
    let mut body = Vec::new();
    append_integer(&mut body, bytes.len() as u32);
    body.extend_from_slice(bytes);
    out.push(0x11);
    let (length, width) = fstart_acpi::aml_linker::package_length(body.len())
        .expect("required CPU AML package exceeds length limit");
    out.extend_from_slice(&length[..width]);
    out.extend_from_slice(&body);
}
fn name_integer(out: &mut Vec<u8>, name: &str, value: u32) {
    append_name(
        out,
        name.as_bytes().try_into().expect("ACPI NameSeg"),
        |out| append_integer(out, value),
    );
}
fn name_string(out: &mut Vec<u8>, name: &str, value: &str) {
    append_name(
        out,
        name.as_bytes().try_into().expect("ACPI NameSeg"),
        |out| {
            out.push(0x0d);
            out.extend_from_slice(value.as_bytes());
            out.push(0);
        },
    );
}
