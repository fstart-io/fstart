//! Fixed Intel CAR/postcar/ramstage reservation model.
//!
//! This is the X61 pre-link geometry boundary, not an initialization graph or
//! authentication policy. The fixed runtime flow consumes its linked descriptor.

use crate::resolved::Span;
use fstart_core::layout::RegionKind;
use fstart_image_build::layout::EncodedLayout;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StageReservation {
    pub image: Span,
    pub writable: Span,
    pub stack: u64,
    pub heap: u64,
}

impl StageReservation {
    fn validate(self) -> Result<(), String> {
        if self.stack == 0 || !self.stack.is_multiple_of(16) || !self.heap.is_multiple_of(16) {
            return Err("stack must be nonzero; stack/heap must be 16-byte aligned".into());
        }
        let occupied = self
            .stack
            .checked_add(self.heap)
            .ok_or("stack/heap overflow")?;
        if occupied >= self.writable.size {
            return Err("stage budgets leave no data/BSS capacity".into());
        }
        Ok(())
    }

    /// Whole future runtime footprint. Bootstrap decoding may also place its
    /// compressed input here, after initialized output and before stage entry.
    pub(crate) fn load_window(self) -> Result<Span, String> {
        if self.image.end()? != self.writable.base {
            return Err("RAM stage image and writable reservations must be adjacent".into());
        }
        let end = self.writable.end()?;
        Ok(Span {
            base: self.image.base,
            size: end - self.image.base,
        })
    }

    pub(crate) fn stack_span(self) -> Span {
        Span {
            base: self.writable.base + self.writable.size - self.stack,
            size: self.stack,
        }
    }

    pub(crate) fn heap_span(self) -> Option<Span> {
        (self.heap != 0).then(|| Span {
            base: self.stack_span().base - self.heap,
            size: self.heap,
        })
    }
}

/// Closed family roles: metadata cannot reorder stages or invent transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum IntelStage {
    Bootblock,
    Postcar,
    Ramstage,
}

impl IntelStage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bootblock => "bootblock",
            Self::Postcar => "postcar",
            Self::Ramstage => "ramstage",
        }
    }
    pub fn environment(self) -> &'static str {
        match self {
            Self::Bootblock => "car",
            Self::Postcar => "postcar",
            Self::Ramstage => "ram",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct IntelReservations {
    pub flash: Span,
    /// Full BIOS mapping used by media identity and postcar ROM caching.
    /// The bootblock occupies its tail; filesystem capacity excludes that tail.
    pub firmware: Span,
    /// Static bootstrap envelope, additionally limited by trained RAM at boot.
    pub bootstrap_ram: Span,
    pub bootblock: StageReservation,
    pub postcar: StageReservation,
    pub ramstage: StageReservation,
    /// Persistent low-memory exclusion including the architecture handoff page.
    pub low_memory: Span,
    /// Boot-media arena, not the bootstrap decoder's appended compressed input.
    pub scratch: Span,
}

impl IntelReservations {
    pub(crate) fn stage(&self, role: IntelStage) -> StageReservation {
        match role {
            IntelStage::Bootblock => self.bootblock,
            IntelStage::Postcar => self.postcar,
            IntelStage::Ramstage => self.ramstage,
        }
    }

    pub fn filesystem_capacity(&self) -> Result<u64, String> {
        self.bootblock
            .image
            .base
            .checked_sub(self.firmware.base)
            .filter(|size| *size != 0)
            .ok_or_else(|| "BIOS mapping leaves no filesystem capacity".into())
    }

    pub fn validate(&self) -> Result<(), String> {
        let ram_regions = [
            ("CAR", self.bootblock.writable),
            ("postcar image", self.postcar.image),
            ("postcar writable", self.postcar.writable),
            ("ramstage image", self.ramstage.image),
            ("ramstage writable", self.ramstage.writable),
            ("low memory", self.low_memory),
            ("scratch", self.scratch),
        ];
        let regions: Vec<_> = [("bootblock image", self.bootblock.image)]
            .into_iter()
            .chain(ram_regions)
            .collect();
        for (name, span) in regions.iter().copied().chain([
            ("flash", self.flash),
            ("firmware", self.firmware),
            ("bootstrap RAM", self.bootstrap_ram),
        ]) {
            let end = span.end().map_err(|e| format!("{name}: {e}"))?;
            if span.base % 4096 != 0 || span.size % 4096 != 0 || end > 0x1_0000_0000 {
                return Err(format!(
                    "{name}: expected page-aligned, below-4-GiB reservation"
                ));
            }
        }
        if self.flash.end()? != 0x1_0000_0000 || self.bootblock.image.end()? != self.flash.end()? {
            return Err("Intel reset image must end at the top of 32-bit flash mapping".into());
        }
        for span in [self.bootblock.image, self.firmware] {
            if !self.flash.contains(span.base, span.size) {
                return Err("bootblock/firmware exceeds flash".into());
            }
        }
        if self.firmware.end()? != self.flash.end()?
            || !self
                .firmware
                .contains(self.bootblock.image.base, self.bootblock.image.size)
        {
            return Err("BIOS mapping must contain the fixed bootblock tail".into());
        }
        self.filesystem_capacity()?;
        for (i, (name, span)) in regions.iter().enumerate() {
            for (other_name, other) in &regions[i + 1..] {
                if span.overlaps(*other) {
                    return Err(format!("{name} overlaps {other_name}"));
                }
            }
        }
        for (name, span) in ram_regions {
            if span.overlaps(self.flash) {
                return Err(format!("{name} overlaps flash"));
            }
        }
        for stage in [self.bootblock, self.postcar, self.ramstage] {
            stage.validate()?;
        }
        if !self.flash.size.is_power_of_two() || !self.flash.base.is_multiple_of(self.flash.size) {
            return Err("flash must fit one aligned ROM MTRR".into());
        }
        for span in [
            self.postcar.load_window()?,
            self.ramstage.load_window()?,
            self.scratch,
        ] {
            if !self.bootstrap_ram.contains(span.base, span.size) {
                return Err("complete stage/scratch reservation exceeds bootstrap RAM".into());
            }
        }
        for span in [self.low_memory, self.bootblock.writable, self.flash] {
            if self.bootstrap_ram.overlaps(span) {
                return Err("bootstrap RAM overlaps low-memory/CAR/flash exclusion".into());
            }
        }
        let car = self.bootblock.writable;
        if !car.size.is_power_of_two() || !car.base.is_multiple_of(car.size) {
            return Err("CAR must be an aligned power-of-two reservation".into());
        }
        if self.low_memory.base != 0 || self.low_memory.size < 0x100000 {
            return Err("low-memory exclusion must cover the handoff/trampoline ABI".into());
        }
        if self.bootblock.image.size <= 4096 {
            return Err("reset page leaves no bootblock code capacity".into());
        }
        if self.bootblock.heap != 0 || self.postcar.heap != 0 {
            return Err("CAR and postcar flows have no allocator heap".into());
        }
        Ok(())
    }

    /// Emit current reservations and fixed-role bootstrap geometry. Consumers
    /// must intersect bootstrap bounds with trained RAM and authenticate bytes;
    /// the layout descriptor does not replace the authenticated bootstrap root.
    pub fn descriptor(&self, role: IntelStage) -> Result<EncodedLayout, String> {
        self.validate()?;
        let (index, stage) = match role {
            IntelStage::Bootblock => (0, self.bootblock),
            IntelStage::Postcar => (1, self.postcar),
            IntelStage::Ramstage => (2, self.ramstage),
        };
        let mut regions = vec![
            stage.image.region(RegionKind::Image),
            stage.writable.region(RegionKind::Writable),
            stage.stack_span().region(RegionKind::Stack),
            self.flash.region(RegionKind::Flash),
            self.firmware.region(RegionKind::Firmware),
            self.low_memory.region(RegionKind::Reserved),
            self.scratch.region(RegionKind::Reserved),
            self.scratch.region(RegionKind::BootMediaScratch),
            self.bootstrap_ram.region(RegionKind::BootstrapRam),
            self.postcar
                .load_window()?
                .region(RegionKind::BootstrapPostcar),
            self.ramstage
                .load_window()?
                .region(RegionKind::BootstrapMainstage),
        ];
        if let Some(heap) = stage.heap_span() {
            regions.push(heap.region(RegionKind::Heap));
        }
        EncodedLayout::encode(index, &regions).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fstart_core::layout::Layout;

    // Candidate capacities for validation, not a second authored board profile.
    pub(crate) fn candidate() -> IntelReservations {
        let span = |base, size| Span { base, size };
        IntelReservations {
            flash: span(0xffc00000, 0x400000),
            firmware: span(0xffe80000, 0x180000),
            bootstrap_ram: span(0x100000, 0x3ff00000),
            bootblock: StageReservation {
                image: span(0xfffc0000, 0x40000),
                writable: span(0xfef00000, 0x80000),
                stack: 0x2000,
                heap: 0,
            },
            postcar: StageReservation {
                image: span(0x1000000, 0x10000),
                writable: span(0x1010000, 0x10000),
                stack: 0x2000,
                heap: 0,
            },
            ramstage: StageReservation {
                image: span(0x4000000, 0x400000),
                writable: span(0x4400000, 0xc00000),
                stack: 0x400000,
                heap: 0x200000,
            },
            low_memory: span(0, 0x100000),
            scratch: span(0x2000000, 0x1000000),
        }
    }

    #[test]
    fn stages_keep_fixed_budgets_and_distinct_wire_identities() {
        let config = candidate();
        assert_eq!(config.filesystem_capacity().unwrap(), 0x140000);
        for (index, role) in [
            IntelStage::Bootblock,
            IntelStage::Postcar,
            IntelStage::Ramstage,
        ]
        .into_iter()
        .enumerate()
        {
            let encoded = config.descriptor(role).unwrap();
            let layout = Layout::parse(encoded.as_bytes()).unwrap();
            assert_eq!(layout.stage_index(), index as u16);
            let stack = layout.region(RegionKind::Stack).unwrap();
            let writable = layout.region(RegionKind::Writable).unwrap();
            assert_eq!(stack.base + stack.size, writable.base + writable.size);
            assert_eq!(layout.region(RegionKind::Heap).is_some(), index == 2);
            for (kind, stage) in [
                (RegionKind::BootstrapPostcar, config.postcar),
                (RegionKind::BootstrapMainstage, config.ramstage),
            ] {
                let window = layout.region(kind).unwrap();
                assert_eq!(window.base, stage.image.base);
                assert_eq!(window.end(), Some(stage.writable.end().unwrap()));
            }
            assert_eq!(
                layout.region(RegionKind::BootMediaScratch).unwrap().base,
                config.scratch.base
            );
        }
    }

    #[test]
    fn rejects_colliding_lifetimes_bad_reset_mapping_and_exhausted_budgets() {
        let mut config = candidate();
        config.scratch = config.postcar.writable;
        assert!(config.validate().unwrap_err().contains("overlaps"));
        let mut config = candidate();
        config.bootblock.image.base -= 4096;
        assert!(config.validate().unwrap_err().contains("reset"));
        let mut config = candidate();
        config.ramstage.stack = config.ramstage.writable.size;
        assert!(config.validate().unwrap_err().contains("capacity"));
        let mut config = candidate();
        config.postcar.image.base = u64::MAX - 4095;
        assert!(config.validate().is_err());
        let mut config = candidate();
        config.bootstrap_ram.size = 0x4700000;
        assert!(config.ramstage.image.end().unwrap() <= config.bootstrap_ram.end().unwrap());
        assert!(config.validate().unwrap_err().contains("complete stage"));
    }
}
