// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed PSP partition operation. Paths are explicit, never discovered.
use super::{Blob, FlashRegion, Key, PackedImage, PicassoImage};
use crate::build_plan::FileBinding;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlob {
    pub key: Key,
    /// Workspace-relative or absolute, exactly as supplied by the build owner.
    pub path: String,
}
/// Explicit packaging inputs, not runtime silicon policy or vendor authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PicassoFiles {
    pub flash_size: u32,
    pub efs_offset: u32,
    pub partition_end: u32,
    pub spi_read_mode: u8,
    pub spi_speed: u8,
    pub micron: bool,
    pub soft_fuse: u64,
    pub psp: Vec<FileBlob>,
    pub bios: Vec<FileBlob>,
    pub apob_nv: Option<FlashRegion>,
    pub psp_nv: Option<FlashRegion>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PicassoAssembly {
    pub reset_unit: String,
    /// Binds the exact configuration origin/bytes into image provenance too.
    pub configuration: FileBinding,
    pub files: PicassoFiles,
    pub reset_destination: u64,
    pub reset_size: u32,
    pub apob_destination: u64,
    pub shared_memory: Option<(u64, u32)>,
}
impl PicassoAssembly {
    pub fn validate(&self) -> Result<(), String> {
        let f = &self.files;
        if self.configuration.environment != "IMAGE_CONFIG"
            || self.configuration.capacity != 65536
            || self.configuration.path.is_empty()
            || self.configuration.path.contains('\0')
            || self.reset_unit.is_empty()
            || self.reset_size == 0
            || self.reset_destination == 0
            || self
                .reset_destination
                .checked_add(u64::from(self.reset_size))
                .is_none_or(|end| end > 0x1_0000_0000 || end & 0xffff != 0)
            || !(0x40000..=0x1000000).contains(&f.flash_size)
            || !f.flash_size.is_power_of_two()
            || f.efs_offset >= f.partition_end
            || f.partition_end > f.flash_size
            || f.spi_read_mode > 7
            || f.spi_speed > 3
            || f.psp.len() > 64
            || f.bios.len() > 64
        {
            return Err("invalid Picasso PSP image operation geometry/budget".into());
        }
        let mut keys = std::collections::BTreeSet::new();
        for (bios, files) in [(false, &f.psp), (true, &f.bios)] {
            for file in files {
                super::validate_key(file.key, bios)?;
                if file.path.is_empty()
                    || file.path.contains('\0')
                    || !keys.insert((bios, file.key))
                {
                    return Err("invalid or duplicate Picasso file declaration".into());
                }
            }
        }
        Ok(())
    }
    /// Stable namespaces are image-only; never installed in compiler environment.
    pub fn bindings(&self) -> Result<Vec<FileBinding>, String> {
        self.validate()?;
        let capacity = u64::from(self.files.partition_end - self.files.efs_offset);
        let mut bindings = vec![self.configuration.clone()];
        for (namespace, files) in [("PSP", &self.files.psp), ("BIOS", &self.files.bios)] {
            bindings.extend(files.iter().map(|file| FileBinding {
                environment: format!(
                    "{namespace}_{:02X}_{:02X}_{:02X}",
                    file.key.kind, file.key.subprogram, file.key.instance
                ),
                path: file.path.clone(),
                capacity,
            }));
        }
        Ok(bindings)
    }
    /// `sources` follows bindings() order, including the configuration witness.
    /// Full format validation precedes output; this does not authenticate bytes.
    pub fn pack(&self, reset: &[u8], sources: &[&[u8]]) -> Result<PackedImage, String> {
        self.validate()?;
        if reset.len() != self.reset_size as usize
            || sources.len() != 1 + self.files.psp.len() + self.files.bios.len()
        {
            return Err("PSP operation reset/source extent mismatch".into());
        }
        if sources[0].len() > self.configuration.capacity as usize
            || serde_json::from_slice::<PicassoFiles>(sources[0]).map_err(|e| e.to_string())?
                != self.files
        {
            return Err("PSP image configuration changed after host resolution".into());
        }
        let total = sources[1..]
            .iter()
            .try_fold(0usize, |sum, bytes| sum.checked_add(bytes.len()))
            .ok_or("PSP input size overflow")?;
        if total > (self.files.partition_end - self.files.efs_offset) as usize {
            return Err("PSP blob inputs exceed partition budget".into());
        }
        let blobs = |files: &[FileBlob], bytes: &[&[u8]]| {
            files
                .iter()
                .zip(bytes)
                .map(|(file, bytes)| Blob {
                    key: file.key,
                    bytes: bytes.to_vec(),
                })
                .collect()
        };
        let split = 1 + self.files.psp.len();
        PicassoImage {
            flash_size: self.files.flash_size,
            efs_offset: self.files.efs_offset,
            partition_end: self.files.partition_end,
            spi_read_mode: self.files.spi_read_mode,
            spi_speed: self.files.spi_speed,
            micron: self.files.micron,
            soft_fuse: self.files.soft_fuse,
            psp: blobs(&self.files.psp, &sources[1..split]),
            bios: blobs(&self.files.bios, &sources[split..]),
            reset_image: reset.to_vec(),
            reset_destination: self.reset_destination,
            apob_destination: self.apob_destination,
            shared_memory: self.shared_memory,
            apob_nv: self.files.apob_nv,
            psp_nv: self.files.psp_nv,
        }
        .pack()
    }
}
