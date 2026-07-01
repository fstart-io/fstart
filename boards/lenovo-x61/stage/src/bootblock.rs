//! Lenovo ThinkPad X61 bootblock recipe binding.

pub type BootblockBoard =
    fstart_platform_intel_gm965_ich8::Gm965Ich8BootblockBoard<crate::board::Board>;
