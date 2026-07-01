//! Lenovo ThinkPad X61 UEFI ramstage recipe binding.

pub type MainBoard = fstart_platform_intel_gm965_ich8::Gm965Ich8RamstageBoard<crate::board::Board>;
