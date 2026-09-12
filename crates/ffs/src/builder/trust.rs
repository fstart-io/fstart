//! Finalize constant policy in raw inputs before any compression/placement.

use super::*;
use fstart_core::ffs::{
    locator::{LOCATOR_SIZE, LocatorBlock},
    trust::{TRUST_SIZE, TrustBlock},
};

/// Identity of raw, policy-patched input bytes handed to compression. Compiler
/// artifacts remain reusable when only packaging policy changes; these hashes
/// identify the actual inputs, not the unpatched cached ELF or flat image.
#[derive(Debug, Clone)]
pub struct PatchedInputIdentity {
    pub region: String,
    pub file: String,
    pub segment: String,
    pub sha256: [u8; 32],
    pub trust_sha256: [u8; 32],
}

pub(super) fn finalize_trust(
    config: &mut FfsImageConfig,
    root: &BootRootConfig,
) -> Result<(Vec<PatchedInputIdentity>, [u8; TRUST_SIZE]), String> {
    if config.keys.len() > TRUST_MAX_KEYS {
        return Err("too many authorized keys".into());
    }
    let mut trust = TrustBlock::placeholder();
    trust.key_count = config.keys.len() as u32;
    trust.keys[..config.keys.len()].copy_from_slice(&config.keys);
    trust.image_family = root.image_family;
    // The current platform policy is explicitly zero, not persistent rollback.
    // It must not be inferred from the signed image's security version.
    trust.minimum_security_version = 0;
    let mut encoded = [0; TRUST_SIZE];
    trust.write_to(&mut encoded);
    let mut placeholder = [0; TRUST_SIZE];
    TrustBlock::placeholder().write_to(&mut placeholder);
    let trust_sha256 = digest::hash_sha256(&encoded);
    let mut identities = Vec::new();
    for region in &mut config.regions {
        match region {
            InputRegion::Container { name, files } => {
                for file in files {
                    patch_segments(
                        name,
                        &file.name,
                        &mut file.segments,
                        &placeholder,
                        &encoded,
                        trust_sha256,
                        &mut identities,
                    )?;
                }
            }
            InputRegion::ContainerWithExternal {
                name,
                files,
                external_files,
                ..
            } => {
                for file in files {
                    patch_segments(
                        name,
                        &file.name,
                        &mut file.segments,
                        &placeholder,
                        &encoded,
                        trust_sha256,
                        &mut identities,
                    )?;
                }
                for file in external_files {
                    patch_segments(
                        name,
                        &file.name,
                        &mut file.segments,
                        &placeholder,
                        &encoded,
                        trust_sha256,
                        &mut identities,
                    )?;
                }
            }
            InputRegion::Raw { .. } | InputRegion::ExternalRaw { .. } => {}
        }
    }
    Ok((identities, encoded))
}

fn patch_segments(
    region: &str,
    file: &str,
    segments: &mut [InputSegment],
    placeholder: &[u8; TRUST_SIZE],
    encoded: &[u8; TRUST_SIZE],
    trust_sha256: [u8; 32],
    identities: &mut Vec<PatchedInputIdentity>,
) -> Result<(), String> {
    for segment in segments {
        if segment.compression != Compression::None {
            for offset in (0..segment.data.len().saturating_sub(LOCATOR_SIZE - 1)).step_by(8) {
                if LocatorBlock::parse(&segment.data[offset..offset + LOCATOR_SIZE]).is_some() {
                    return Err(std::format!(
                        "compressed file '{file}' contains a mutable locator"
                    ));
                }
            }
        }
        for offset in (0..segment.data.len().saturating_sub(TRUST_SIZE - 1)).step_by(8) {
            let bytes = &mut segment.data[offset..offset + TRUST_SIZE];
            if bytes == placeholder {
                bytes.copy_from_slice(encoded);
            } else if bytes[..16] == placeholder[..16] && bytes != encoded {
                return Err(std::format!(
                    "file '{file}' contains a stale or malformed constant policy"
                ));
            }
        }
        identities.push(PatchedInputIdentity {
            region: region.into(),
            file: file.into(),
            segment: segment.name.clone(),
            sha256: digest::hash_sha256(&segment.data),
            trust_sha256,
        });
    }
    Ok(())
}
