//! Build-script shim for fstart-platform-sp3: declares the payload cfg fbuild
//! sets and forwards the SMM handler image when one
//! is built, after checking that it fits the TSEG (`host::smm_unit`).

use fstart_smm::{CpuSmmLayout, SMM_IDENTITY_TABLE_SIZE, SmmImageHeader, SmramLayout};

/// Save-state bytes per SMBASE window `fstart_arch`'s AMD64 installer
/// reserves (`cpu::amd::smm::SAVE_STATE_SIZE`; AMD64 uses the top 0x200).
const AMD_SAVE_STATE_SIZE: u32 = 0x400;

fn main() {
    println!(
        "cargo:rustc-check-cfg=cfg(fstart_payload, values(\"halt\", \"linux\", \"crabefi\", \"coreboot\"))"
    );
    println!("cargo:rustc-check-cfg=cfg(fstart_sp3_has_smm_image)");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_TSEG_SIZE");
    println!("cargo:rerun-if-env-changed=FSTART_MP_MAX_CPUS");
    if let Ok(image) = std::env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={image}");
        if let Err(e) = check_smm_image(&image) {
            panic!("SMM image {image}: {e}");
        }
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={image}");
        println!("cargo:rustc-cfg=fstart_sp3_has_smm_image");
    }
}

fn env<T: std::str::FromStr>(name: &str) -> Result<T, String> {
    std::env::var(name)
        .map_err(|_| format!("{name} is not set (the host plan sets it)"))?
        .parse()
        .map_err(|_| format!("{name} is not a number"))
}

/// Lay the image out as the installer will for `FSTART_MP_MAX_CPUS`
/// threads in `FSTART_SMM_TSEG_SIZE` bytes, so an image that cannot be
/// installed fails the build instead of halting the boot at step 0x58.
fn check_smm_image(path: &str) -> Result<(), String> {
    let tseg: u64 = env("FSTART_SMM_TSEG_SIZE")?;
    let cpus: u16 = env("FSTART_MP_MAX_CPUS")?;
    let image = std::fs::read(path).map_err(|e| e.to_string())?;
    let header = SmmImageHeader::parse(&image).map_err(|e| format!("{e:?}"))?;
    if header.entry_count < cpus {
        return Err(format!(
            "{} entry stubs for {cpus} CPUs",
            header.entry_count
        ));
    }
    let stub = (0..cpus)
        .map(|i| header.entry(&image, i).map(|e| e.stub_size))
        .try_fold(0, |max, size| size.map(|s| max.max(s)))
        .map_err(|e| format!("{e:?}"))?;
    let layout = SmramLayout {
        // Any TSEG-aligned base gives the same placement.
        smram_base: 1 << 31,
        smram_size: tseg,
        entry_count: cpus,
        save_state_size: AMD_SAVE_STATE_SIZE,
        stack_size: header.stack_size,
        entry_stub_size: stub,
        handler_mem_size: header.handler_mem_size,
        page_table_size: SMM_IDENTITY_TABLE_SIZE,
    };
    let zero = CpuSmmLayout {
        smbase: 0,
        entry_addr: 0,
        save_state_base: 0,
        save_state_top: 0,
        stack_bottom: 0,
        stack_top: 0,
    };
    let mut out = vec![zero; usize::from(cpus)];
    fstart_smm::compute_cpu_layout(&layout, &mut out).map_err(|e| {
        format!(
            "does not fit a {tseg:#x} TSEG for {cpus} CPUs ({e:?}; handler {:#x}, stacks {:#x} each)",
            header.handler_mem_size, header.stack_size
        )
    })?;
    Ok(())
}
