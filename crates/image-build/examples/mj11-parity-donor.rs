//! CI fixture only: an MJ11 donor with real directory geometry and inert data.
//! No authentic PSP firmware or microcode; neither donor nor output can boot.
use fstart_image_build::{build_plan::ImageOperation, bundle::Bundle};
use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let bundle = Bundle::decode(&fs::read(args.next().ok_or("expected MJ11 bundle")?)?)?;
    let out = args.next().ok_or("expected fixture output path")?;
    let manifest = bundle.manifest;
    assert_eq!(manifest.board, "gigabyte-mj11-ec1");
    let mut image = manifest
        .plan
        .images
        .into_iter()
        .find_map(|step| match step.operation {
            ImageOperation::AmdEfs(image) => Some(image),
            _ => None,
        })
        .ok_or("MJ11 bundle has no EFS recipe")?;
    // No FFS in the fake donor. Its otherwise empty window holds the local
    // file formats the extractor looks for outside the PSP/BIOS directories.
    let window = image.ffs.take().ok_or("MJ11 bundle has no FFS window")?;
    let bindings = image.bindings()?;
    let blobs: Vec<_> = bindings.iter().map(|b| b.path.as_bytes()).collect();
    let mut donor = image.assemble(&blobs, &vec![0x55; image.reset_size as usize], None)?;
    assert!(donor.is_whole_flash(image.flash_size));
    let mut free = &mut donor.bytes[window.offset as usize..(window.offset + window.size) as usize];
    let tables = manifest
        .packed_donor
        .ok_or("missing MJ11 blob table")?
        .tables;
    let [(soc, _)] = tables.as_slice() else {
        return Err("expected one MJ11 SoC".into());
    };
    assert!(soc.vbios.is_empty());
    for entry in &soc.microcode {
        let size = fstart_amdfw_format::microcode::patch_size(entry.equivalence_id)
            .ok_or("unknown test patch size")?;
        let (dummy, rest) = free
            .split_at_mut_checked(size)
            .ok_or("test patches exceed FFS window")?;
        dummy.fill(0);
        dummy[4] = 1; // Nonzero patch ID; the payload is intentionally all zero.
        dummy[0x18..0x1a].copy_from_slice(&entry.equivalence_id.to_le_bytes());
        free = rest;
    }
    fs::write(out, donor.bytes)?;
    Ok(())
}
