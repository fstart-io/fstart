//! Main-stage fixed-flow adapter.

use fstart_stage::fixed_helpers::{set_sunxi_handoff_ptr, SunxiMainBoard};

use crate::common::SunxiBoard;

pub fn set_handoff_ptr(ptr: usize) {
    set_sunxi_handoff_ptr(ptr);
}

pub type MainBoard = SunxiMainBoard<SunxiBoard>;
