// SPDX-License-Identifier: MIT OR Apache-2.0
//! Patch compiled reset-stage metadata from the finalized FFS. The locator
//! encoding is a stage ABI, independent of silicon or vendor-bank topology.
use fstart_amdfw_format::anchor::FlashAnchor;
use fstart_core::ffs::{
    ANCHOR_SIZE, AnchorBlock,
    locator::{LOCATOR_SIZE, LocatorBlock},
    trust::{TRUST_SIZE, TrustBlock},
};
use serde::{Deserialize, Serialize};
use zerocopy::IntoBytes;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageLocator {
    /// Fixed-window initial stages embed the FFS anchor and require both sites.
    #[default]
    Anchor,
    /// Bank-relative stages carry a locator plus a flash-window anchor. Sites
    /// not linked by an early bring-up stage are optional; duplicates are errors.
    FlashAnchor,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Patched {
    pub trust: bool,
    pub locator: bool,
    pub flash: bool,
}

fn site(
    bytes: &[u8],
    marker: &[u8],
    alignment: usize,
    required: bool,
) -> Result<Option<usize>, String> {
    let mut sites = (0..bytes.len().saturating_sub(marker.len() - 1))
        .step_by(alignment)
        .filter(|&at| bytes[at..at + marker.len()] == *marker);
    let first = sites.next();
    if sites.next().is_some() || (required && first.is_none()) {
        return Err("no unique reset-stage patch site".into());
    }
    Ok(first)
}
fn finalized_trust(ffs: &[u8], placeholder: &[u8]) -> Option<[u8; TRUST_SIZE]> {
    (0..ffs.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .map(|at| &ffs[at..at + TRUST_SIZE])
        .filter(|bytes| *bytes != placeholder)
        .find_map(|bytes| TrustBlock::parse(bytes).filter(|t| t.key_count.get() > 0))
        .map(|trust| {
            let mut out = [0; TRUST_SIZE];
            trust.write_to(&mut out);
            out
        })
}
fn locator(ffs: &[u8]) -> Option<&[u8]> {
    (0..ffs.len().saturating_sub(LOCATOR_SIZE - 1))
        .step_by(8)
        .map(|at| (at, &ffs[at..at + LOCATOR_SIZE]))
        .find(|(at, bytes)| {
            LocatorBlock::parse(bytes).is_some_and(|l| {
                l.total_image_size.get() as usize == ffs.len()
                    && l.anchor_offset.get() as usize == *at
            })
        })
        .map(|(_, bytes)| bytes)
}

pub fn patch_reset_image(
    reset: &mut [u8],
    ffs: &[u8],
    flash: FlashAnchor,
    format: StageLocator,
) -> Result<Patched, String> {
    let required = format == StageLocator::Anchor;
    let mut trust_placeholder = [0; TRUST_SIZE];
    TrustBlock::placeholder().write_to(&mut trust_placeholder);
    let trust = site(reset, &trust_placeholder, 8, required)?
        .map(|at| {
            let trust = finalized_trust(ffs, &trust_placeholder)
                .ok_or("FFS carries no finalized trust policy")?;
            reset[at..at + TRUST_SIZE].copy_from_slice(&trust);
            Ok::<_, String>(())
        })
        .transpose()?
        .is_some();
    let (placeholder, alignment) = match format {
        StageLocator::Anchor => {
            let mut bytes = vec![0; ANCHOR_SIZE];
            AnchorBlock::placeholder().write_to(&mut bytes);
            (bytes, 1)
        }
        StageLocator::FlashAnchor => {
            let mut bytes = vec![0; LOCATOR_SIZE];
            LocatorBlock::placeholder().write_to(&mut bytes);
            (bytes, 8)
        }
    };
    let locator = site(reset, &placeholder, alignment, required)?
        .map(|at| {
            let value = match format {
                StageLocator::Anchor => {
                    let at = fstart_ffs::FfsReader::new(ffs)
                        .scan_for_anchor()
                        .map_err(|e| format!("FFS anchor not found: {e:?}"))?;
                    &ffs[at..at + ANCHOR_SIZE]
                }
                StageLocator::FlashAnchor => locator(ffs).ok_or("FFS has no locator")?,
            };
            reset[at..at + value.len()].copy_from_slice(value);
            Ok::<_, String>(())
        })
        .transpose()?
        .is_some();
    let flash = if format == StageLocator::FlashAnchor {
        if FlashAnchor::parse(flash.as_bytes()).is_none() {
            return Err("invalid flash anchor".into());
        }
        site(reset, FlashAnchor::placeholder().as_bytes(), 8, false)?
            .map(|at| {
                reset[at..at + flash.as_bytes().len()].copy_from_slice(flash.as_bytes());
            })
            .is_some()
    } else {
        false
    };
    Ok(Patched {
        trust,
        locator,
        flash,
    })
}
