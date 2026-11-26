//! Forwards the SMM handler image when one
//! is built, after checking that it fits the TSEG (`host::smm_unit`).

use fstart_smm::{
    CpuSmmLayout, SMM_IDENTITY_TABLE_SIZE, SMM_MAPPED_TABLES_MAX, SmmImageHeader, SmramLayout,
};

/// Save-state bytes per SMBASE window `fstart_arch`'s AMD64 installer
/// reserves (`cpu::amd::smm::SAVE_STATE_SIZE`; AMD64 uses the top 0x200).
const AMD_SAVE_STATE_SIZE: u32 = 0x400;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fstart_amd_has_smm_image)");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_TSEG_SIZE");
    println!("cargo:rerun-if-env-changed=FSTART_MP_MAX_CPUS");
    ras_capacity();
    if let Ok(image) = std::env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={image}");
        // A host plan that sets the TSEG size has the image's fit checked.
        if std::env::var_os("FSTART_SMM_TSEG_SIZE").is_some()
            && let Err(e) = check_smm_image(&image)
        {
            panic!("SMM image {image}: {e}");
        }
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={image}");
        println!("cargo:rustc-cfg=fstart_amd_has_smm_image");
    }
}

/// The SMM RAS handler's per-CPU tables (`smm::ras`) for the board's CPU
/// ceiling (`FSTART_MP_MAX_CPUS`, which the host plan sets for the stage
/// and the SMM image alike), never below what every board had before: 512
/// CPUs and 8 thresholding blocks each. Only a board with more CPUs (2P
/// Venice) pays for more SMRAM.
fn ras_capacity() {
    const FLOOR: usize = 512;
    let cpus = std::env::var("FSTART_MP_MAX_CPUS")
        .ok()
        .and_then(|cpus| cpus.parse::<usize>().ok())
        .unwrap_or(FLOOR)
        .max(FLOOR);
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR set by Cargo"));
    std::fs::write(
        out.join("ras_capacity.rs"),
        format!(
            "/// Logical CPUs the handler tracks (`build.rs`).\n\
             const MAX_CPUS: usize = {cpus};\n\
             /// Thresholding blocks SMM can own (`build.rs`).\n\
             const BLOCKS: usize = {};\n",
            8 * cpus
        ),
    )
    .expect("ras_capacity.rs");
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
        // With the most tables MMIO above 4 GiB may add (SP7 maps its ECAM
        // and root bridge windows, `sp7::smm::smm_reach`).
        page_table_size: SMM_IDENTITY_TABLE_SIZE + SMM_MAPPED_TABLES_MAX,
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
