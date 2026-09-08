//! Legacy linker entry points. Migrated platforms emit concrete scripts on the host.
pub use fstart_image_build::linker::{generate_linker_script, layout_section, resolved_intel};

pub fn resolved_xip(layout: &crate::resolved::ResolvedBuild) -> Result<String, String> {
    fstart_image_build::linker::resolved_xip(&layout.linker_layout()?)
}
