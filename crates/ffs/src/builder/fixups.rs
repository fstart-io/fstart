//! Bounded fixed-size packaging edits, resolved after layout and before sealing.
use super::*;

#[derive(Debug, Clone)]
pub struct FileAlignment {
    pub file: String,
    pub alignment: usize,
}
#[derive(Debug, Clone)]
pub struct FileLayout {
    pub file: String,
    /// Sole segment's stored-data offset in the logical image, NOT the
    /// physical SPI device offset or CPU address (nor an entry's padding).
    pub offset: u32,
    pub file_type: FileType,
    pub load_addr: u64,
    pub external: bool,
    pub data: Vec<u8>,
}
#[derive(Debug, Clone)]
pub struct FilePatch {
    pub file: String,
    /// Byte offset within the selected file's sole uncompressed segment.
    pub offset: usize,
    pub expected: Vec<u8>,
    pub bytes: Vec<u8>,
}
#[derive(Debug, Clone)]
pub struct FinalizedExternalFile {
    pub file: String,
    /// Placement in the logical image, independent of its device mapping.
    pub offset: u32,
    pub data: Vec<u8>,
}

pub(super) fn alignment(file: &InputFile, alignments: &[FileAlignment]) -> Result<usize, String> {
    let mut requested = alignments.iter().filter(|a| a.file == file.name);
    let base = if file.file_type == FileType::CpuMicrocode {
        16
    } else {
        8
    };
    let Some(value) = requested.next() else {
        return Ok(base);
    };
    if requested.next().is_some()
        || !value.alignment.is_power_of_two()
        || value.alignment < base
        || value.alignment > 4096
    {
        return Err(format!("invalid/duplicate alignment for '{}'", file.name));
    }
    Ok(value.alignment)
}

pub(super) fn layouts(
    image: &[u8],
    manifest: &ImageManifest,
    config: &FfsImageConfig,
) -> Result<Vec<FileLayout>, String> {
    let mut result = Vec::new();
    for region in &manifest.regions {
        let RegionContent::Container { children } = &region.content else {
            continue;
        };
        for entry in children {
            let EntryContent::File {
                file_type,
                segments,
                ..
            } = &entry.content
            else {
                continue;
            };
            if segments.len() != 1 {
                continue;
            }
            let segment = &segments[0];
            if segment.compression != Compression::None
                || !segment.flags.read
                || segment.flags.write
                || segment.stored_size != segment.initialized_size
                || segment.loaded_size != segment.initialized_size
            {
                continue;
            }
            let offset = region
                .offset
                .checked_add(entry.offset)
                .and_then(|v| v.checked_add(segment.offset))
                .ok_or("file offset overflow")?;
            let external = config.regions.iter().find_map(|r| match r {
                InputRegion::ContainerWithExternal {
                    name,
                    external_files,
                    ..
                } if name == region.name.as_str() => external_files
                    .iter()
                    .find(|f| f.name == entry.name.as_str()),
                _ => None,
            });
            let data = if let Some(file) = external {
                file.segments[0].data.clone()
            } else {
                let end = (offset as usize)
                    .checked_add(segment.stored_size as usize)
                    .ok_or("file size overflow")?;
                image
                    .get(offset as usize..end)
                    .ok_or("file outside image")?
                    .to_vec()
            };
            result.push(FileLayout {
                file: entry.name.as_str().into(),
                offset,
                file_type: *file_type,
                load_addr: segment.load_addr,
                external: external.is_some(),
                data,
            });
        }
    }
    Ok(result)
}

pub(super) fn apply(
    image: &mut [u8],
    manifest: &ImageManifest,
    config: &mut FfsImageConfig,
    files: &[FileLayout],
    patches: &[FilePatch],
) -> Result<Vec<usize>, String> {
    // Validate the complete edit set before mutating even an unsigned draft.
    for (index, patch) in patches.iter().enumerate() {
        let mut matches = files.iter().filter(|f| f.file == patch.file);
        let file = matches
            .next()
            .ok_or("fixup file is not flat, read-only and uncompressed")?;
        let named = manifest
            .regions
            .iter()
            .filter_map(|r| match &r.content {
                RegionContent::Container { children } => Some(children),
                _ => None,
            })
            .flatten()
            .filter(|e| e.name.as_str() == patch.file)
            .count();
        if matches.next().is_some() || named != 1 {
            return Err("ambiguous fixup file".into());
        }
        let end = patch
            .offset
            .checked_add(patch.bytes.len())
            .ok_or("fixup overflow")?;
        if patch.bytes.is_empty()
            || patch.bytes.len() != patch.expected.len()
            || file.data.get(patch.offset..end) != Some(patch.expected.as_slice())
        {
            return Err("fixup must preserve size and match expected bytes".into());
        }
        for previous in &patches[..index] {
            if previous.file == patch.file
                && patch.offset < previous.offset + previous.bytes.len()
                && previous.offset < end
            {
                return Err("overlapping fixups".into());
            }
        }
    }
    let mut changed = Vec::new();
    for patch in patches {
        let file = files.iter().find(|f| f.file == patch.file).unwrap();
        let start = file.offset as usize + patch.offset;
        let end = patch.offset + patch.bytes.len();
        if !file.external {
            image[start..start + patch.bytes.len()].copy_from_slice(&patch.bytes);
            changed.push(start);
        }
        for region in &mut config.regions {
            let segments = match region {
                InputRegion::Container { files, .. }
                | InputRegion::ContainerWithExternal { files, .. }
                    if !file.external =>
                {
                    files
                        .iter_mut()
                        .find(|f| f.name == patch.file)
                        .map(|f| &mut f.segments)
                }
                InputRegion::ContainerWithExternal { external_files, .. } if file.external => {
                    external_files
                        .iter_mut()
                        .find(|f| f.name == patch.file)
                        .map(|f| &mut f.segments)
                }
                _ => None,
            };
            if let Some(segments) = segments {
                segments[0].data[patch.offset..end].copy_from_slice(&patch.bytes);
            }
        }
    }
    Ok(changed)
}
