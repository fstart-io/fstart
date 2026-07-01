//! Bootblock fixed-flow adapter.

use fstart_stage::fixed_helpers::SunxiBootblockBoard;

use crate::common::SunxiBoard;

pub type BootblockBoard = SunxiBootblockBoard<SunxiBoard>;
