//! Lenovo ThinkPad X200 SMM: the ICH's own SMI sources only. The EC's events
//! reach the OS as SCIs and the dock is controlled through EC/GPIO, not SMM.

fstart_platform_intel::smm_default_bin!();
