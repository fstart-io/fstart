// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host-side compiler inputs of an Rs reset image, shared by Genoa and
//! Siena: the explicit BIOS-directory microcode patch and the ROM-resident
//! Linux payload. Bytes are bound unchanged by content, never looked up at
//! run time or from a machine-private default.
extern crate std;
use fstart_core::layout::Region;
use fstart_image_build::amd::reset;
use fstart_image_build::build_plan::{CargoTarget, CompilationUnit, FileBinding, UnitOutput};
use fstart_image_build::plan::BuildSelection;
use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::{format, vec, vec::Vec};

/// The name of the reset image's stage unit.
pub const STAGE: &str = "stage";

/// One flat patch for this SoC (`rs_mp` loads it on every thread).
pub const MICROCODE: &str = "FSTART_RS_MICROCODE";
/// The ROM-resident bzImage (`rs_linux`).
pub const LINUX: &str = "FSTART_RS_LINUX";
pub const LINUX_ARGS: &str = "FSTART_RS_LINUX_ARGS";
pub const LINUX_MTRRS: &str = "FSTART_RS_LINUX_MTRRS";

/// What the SoC accepts.
pub struct Inputs<'a> {
    pub soc: &'a str,
    /// Microcode equivalence-ID family byte (`0xa1` Genoa, `0xaa` Siena).
    pub equivalence: u8,
    /// Linux requires the microcode input (the PSP-installed level is not
    /// an OS-ready CPU set on this SoC).
    pub linux_requires_microcode: bool,
    /// Upper bound of the bzImage inside the reset image.
    pub linux_capacity: u64,
    pub default_bootargs: &'a str,
}

/// Bind the selection's compiler inputs and Linux options into `unit`,
/// which must be the halt-only unit of [`stage_unit`].
pub fn bind(
    unit: &mut CompilationUnit,
    selection: &BuildSelection,
    inputs: &Inputs<'_>,
) -> Result<(), String> {
    let soc = inputs.soc;
    let payload = selection.payload.as_deref().unwrap_or(DEFAULT_PAYLOAD);
    if !matches!(payload, "halt" | "uefi" | "linux") {
        return Err(format!(
            "{soc} supports only the halt, uefi or linux payloads"
        ));
    }
    if selection.x86_linux_kernel_load_addr.is_some() {
        return Err(format!(
            "{soc} places the bzImage through the shared e820 loader, not an address override"
        ));
    }
    let (microcode, rest): (Vec<_>, Vec<_>) = selection
        .compiler_files
        .iter()
        .partition(|input| input.environment == MICROCODE);
    let (kernels, other): (Vec<_>, Vec<_>) = rest
        .into_iter()
        .partition(|input| input.environment == LINUX);
    if let Some(input) = other.first() {
        return Err(format!(
            "{soc} accepts only {MICROCODE}/{LINUX} compiler inputs, not {}",
            input.environment
        ));
    }
    match microcode.as_slice() {
        [] if payload == "linux" && inputs.linux_requires_microcode => {
            return Err(format!(
                "{soc} Linux requires an explicit {MICROCODE} input"
            ));
        }
        [] => {}
        [input] => {
            let bytes = std::fs::read(&input.path)
                .map_err(|error| format!("microcode {}: {error}", input.path))?;
            let id = fstart_amdfw_format::microcode::equivalence_id(&bytes)
                .filter(|id| (id >> 8) as u8 == inputs.equivalence)
                .ok_or(format!("microcode must be one flat {soc} patch"))?;
            if fstart_amdfw_format::microcode::patch_size(id) != Some(bytes.len()) {
                return Err(format!("microcode must be exactly one flat {soc} patch"));
            }
            // The CPUID signature the equivalence ID names; the runtime still
            // checks the actual CPU before submitting the load.
            let signature = 0xf00 | (u32::from(id & 0xff00) << 8) | u32::from(id & 0xff);
            fstart_driver_amd::ccx::microcode::Patch::parse(&bytes, signature)
                .map_err(|error| format!("invalid {soc} microcode: {error:?}"))?;
            unit.file_bindings.push(FileBinding {
                environment: input.environment.clone(),
                path: input.path.clone(),
                capacity: bytes.len() as u64,
            });
        }
        _ => return Err(format!("at most one {MICROCODE} input")),
    }
    if payload != "linux" {
        if !kernels.is_empty() || selection.has_x86_linux_overrides() {
            return Err(format!("{LINUX} and Linux options require --payload linux"));
        }
        if payload == "uefi" {
            unit.features.push("payload-uefi".into());
        }
        return Ok(());
    }
    let [kernel] = kernels.as_slice() else {
        return Err(format!(
            "{soc} Linux requires one {LINUX}=/path/to/bzImage input"
        ));
    };
    let bytes =
        std::fs::read(&kernel.path).map_err(|error| format!("Linux {}: {error}", kernel.path))?;
    validate_linux(&bytes, inputs.linux_capacity)?;
    let args = selection
        .x86_linux_bootargs
        .as_deref()
        .unwrap_or(inputs.default_bootargs);
    if args.len() > 2047 || args.contains('\0') {
        return Err("Linux command line exceeds 2047 bytes or contains NUL".into());
    }
    unit.file_bindings.push(FileBinding {
        environment: kernel.environment.clone(),
        path: kernel.path.clone(),
        capacity: inputs.linux_capacity,
    });
    unit.environment_values
        .insert(LINUX_ARGS.into(), args.into());
    unit.environment_values.insert(
        LINUX_MTRRS.into(),
        format!("{}", u8::from(selection.x86_linux_print_mtrrs)),
    );
    unit.features.push("payload-linux".into());
    Ok(())
}

/// A bounded x86-64 bzImage with ACPI RSDP protocol support.
pub fn validate_linux(bytes: &[u8], capacity: u64) -> Result<(), String> {
    if bytes.len() < 4096
        || bytes.len() as u64 > capacity
        || &bytes[0x202..0x206] != b"HdrS"
        || u16::from_le_bytes([bytes[0x206], bytes[0x207]]) < 0x20e
        || u16::from_le_bytes([bytes[0x236], bytes[0x237]]) & 1 == 0
    {
        return Err("expected a bounded x86-64 bzImage with ACPI RSDP protocol support".into());
    }
    let sectors = if bytes[0x1f1] == 0 {
        4
    } else {
        usize::from(bytes[0x1f1])
    };
    let setup = (sectors + 1) * 512;
    let protected = u32::from_le_bytes(bytes[0x1f4..0x1f8].try_into().unwrap()) as usize * 16;
    if protected == 0
        || setup
            .checked_add(protected)
            .is_none_or(|end| end > bytes.len())
    {
        return Err("truncated Linux image".into());
    }
    Ok(())
}

/// The payload without `--payload`: CrabEFI, which finds the boot devices.
pub const DEFAULT_PAYLOAD: &str = "uefi";

/// The reset image's stage unit through the shared AMD reset entry: the
/// platform's `bundle-reset` stage, halting unless [`bind`] adds a payload.
/// `extra` are the platform's descriptor regions besides the entry's own.
pub fn stage_unit(
    layout: &reset::Layout,
    extra: &[Region],
    max_cpus: usize,
) -> Result<CompilationUnit, String> {
    if max_cpus == 0 {
        return Err("Rs CPU capacity must be nonzero".into());
    }
    let descriptor = layout.descriptor(extra)?;
    Ok(CompilationUnit {
        name: STAGE.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        features: vec!["bundle-reset".into()],
        build_std: Some("core,alloc".into()),
        release_only: false,
        rustflags: [
            "-Zub-checks=no",
            "-Crelocation-model=static",
            "-Ccode-model=small",
            "-Clink-arg=-no-pie",
        ]
        .map(String::from)
        .to_vec(),
        linker_script: Some(layout.linker_script(&descriptor)?),
        environment_values: BTreeMap::from([("FSTART_MP_MAX_CPUS".into(), max_cpus.to_string())]),
        bindings: vec![],
        file_bindings: vec![],
        board_features: vec![],
        output: UnitOutput::Executable {
            expectations: layout.expectations(&descriptor)?,
            load_address: layout.image.base,
            flat_capacity: layout.image.size,
        },
    })
}

/// Add the board's SMM unit ([`crate::smm_unit`]) to the reset `unit`: the
/// stage embeds its image (`rs_smm`). Returns the SMM unit.
pub fn bind_smm(unit: &mut CompilationUnit, max_cpus: u16) -> CompilationUnit {
    unit.bindings.push(crate::smm_unit::binding());
    crate::smm_unit::unit(max_cpus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_image_build::plan::CompilerFileSelection;
    use std::{env, fs, path::PathBuf, vec};

    fn file(name: &str, bytes: &[u8]) -> String {
        let path: PathBuf = env::temp_dir().join(format!("rs-host-{}-{name}", std::process::id()));
        fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }
    fn bzimage() -> Vec<u8> {
        let mut b = vec![0u8; 8192];
        b[0x1f1] = 4;
        b[0x1f4..0x1f8].copy_from_slice(&64u32.to_le_bytes());
        b[0x202..0x206].copy_from_slice(b"HdrS");
        b[0x206..0x208].copy_from_slice(&0x20fu16.to_le_bytes());
        b[0x236] = 1;
        b
    }
    fn unit() -> CompilationUnit {
        let layout = reset::Layout {
            image: fstart_image_build::plan::Span {
                base: 0x0a00_0000,
                size: 0x80_0000,
            },
            early: fstart_image_build::plan::Span {
                base: 0x0800_0000,
                size: 0x200_0000,
            },
            data: fstart_image_build::plan::Span {
                base: 0x0801_0000,
                size: 0xf_0000,
            },
            heap: fstart_image_build::plan::Span {
                base: 0x0810_0000,
                size: 0x1e0_0000,
            },
            stack: fstart_image_build::plan::Span {
                base: 0x09f0_0000,
                size: 0x10_0000,
            },
            pages: fstart_image_build::plan::Span {
                base: 0x0800_0000,
                size: 0x9000,
            },
        };
        stage_unit(&layout, &[], 8).unwrap()
    }
    const SIENA: Inputs = Inputs {
        soc: "Siena",
        equivalence: 0xaa,
        linux_requires_microcode: false,
        linux_capacity: 0x60_0000,
        default_bootargs: "console=ttyS0,115200",
    };

    #[test]
    fn linux_and_microcode_are_explicit_bounded_and_soc_checked() {
        let kernel = file("bz", &bzimage());
        let mut patch = vec![0u8; 0x15c0];
        patch[4..8].copy_from_slice(&0x0aa0_0219u32.to_le_bytes());
        patch[0x18..0x1a].copy_from_slice(&0xaa02u16.to_le_bytes());
        let siena = file("aa02", &patch);
        patch[0x18..0x1a].copy_from_slice(&0xa110u16.to_le_bytes());
        let genoa = file("a110", &patch);
        let selection = BuildSelection {
            payload: Some("linux".into()),
            compiler_files: vec![
                CompilerFileSelection {
                    environment: LINUX.into(),
                    path: kernel.clone(),
                },
                CompilerFileSelection {
                    environment: MICROCODE.into(),
                    path: siena,
                },
            ],
            ..Default::default()
        };
        let mut u = unit();
        bind(&mut u, &selection, &SIENA).unwrap();
        assert_eq!(u.file_bindings.len(), 2);
        assert!(u.features.iter().any(|f| f == "payload-linux"));
        assert_eq!(u.environment_values[LINUX_ARGS], "console=ttyS0,115200");
        // Another SoC's patch, a missing kernel and stray inputs are refused.
        let mut wrong = selection.clone();
        wrong.compiler_files[1].path = genoa;
        assert!(bind(&mut unit(), &wrong, &SIENA).is_err());
        let mut uefi = selection.clone();
        uefi.payload = None;
        assert!(bind(&mut unit(), &uefi, &SIENA).is_err());
        // Without a kernel: CrabEFI by default, a real halt on request.
        let mut u = unit();
        bind(&mut u, &BuildSelection::default(), &SIENA).unwrap();
        assert!(u.features.iter().any(|f| f == "payload-uefi"));
        let mut u = unit();
        let halt = BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        };
        bind(&mut u, &halt, &SIENA).unwrap();
        assert!(!u.features.iter().any(|f| f.starts_with("payload-")));
        let mut stray = selection.clone();
        stray.compiler_files[0].environment = "SIENA_LINUX_KERNEL".into();
        assert!(bind(&mut unit(), &stray, &SIENA).is_err());
        let mut placed = selection;
        placed.x86_linux_kernel_load_addr = Some(0x100_0000);
        assert!(bind(&mut unit(), &placed, &SIENA).is_err());
        let required = Inputs {
            linux_requires_microcode: true,
            ..SIENA
        };
        let only_kernel = BuildSelection {
            payload: Some("linux".into()),
            compiler_files: vec![CompilerFileSelection {
                environment: LINUX.into(),
                path: kernel,
            }],
            ..Default::default()
        };
        assert!(bind(&mut unit(), &only_kernel, &SIENA).is_ok());
        assert!(bind(&mut unit(), &only_kernel, &required).is_err());
    }
}
