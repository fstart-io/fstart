//! Absolute ACPI namespace paths, derived at compile time.
//!
//! Like coreboot's `acpi_device_path()`, a device's path is its parent's
//! path plus the name its driver gives it: a northbridge names the PCI root
//! under `\_SB`, a southbridge its LPC bridge on that root, an EC itself on
//! the LPC bridge. [`AcpiPath::child`] is `const`, so the paths are built
//! during compilation and spliced into `acpi_dsl!` with `#{const path N
//! EXPR}` and `#{const name EXPR}` without runtime code.
//!
//! ```
//! use fstart_acpi::AcpiPath;
//! use fstart_acpi_macros::acpi_dsl;
//! const LPC: AcpiPath = AcpiPath::SB.child("PCI0").child("LPCB");
//! let _ = acpi_dsl! { Notify(#{const path 4 LPC.child("EC")}, 0x80u32); };
//! ```
//!
//! `N` is the path's depth; another depth fails to compile:
//!
//! ```compile_fail
//! use fstart_acpi::AcpiPath;
//! use fstart_acpi_macros::acpi_dsl;
//! let _ = acpi_dsl! { Scope(#{const path 2 AcpiPath::SB}) { } };
//! ```

/// Deepest path an [`AcpiPath`] holds.
pub const MAX_DEPTH: usize = 8;

const CAPACITY: usize = 1 + MAX_DEPTH * 5;

/// An absolute namespace path such as `\_SB_.PCI0.LPCB`, every segment
/// padded to four characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcpiPath {
    text: [u8; CAPACITY],
    len: u8,
    depth: u8,
}

impl AcpiPath {
    /// The namespace root, `\`.
    pub const ROOT: Self = {
        let mut text = [0; CAPACITY];
        text[0] = b'\\';
        Self {
            text,
            len: 1,
            depth: 0,
        }
    };
    /// The system bus, `\_SB_`.
    pub const SB: Self = Self::ROOT.child("_SB");

    /// This path's child `name`: one to four characters, padded with `_`.
    /// Panics (a compile error in const context) on an invalid NameSeg.
    #[must_use]
    pub const fn child(self, name: &str) -> Self {
        let segment = name_seg(name);
        assert!((self.depth as usize) < MAX_DEPTH, "ACPI path too deep");
        let mut path = self;
        let mut at = path.len as usize;
        if path.depth > 0 {
            path.text[at] = b'.';
            at += 1;
        }
        let mut index = 0;
        while index < 4 {
            path.text[at + index] = segment[index];
            index += 1;
        }
        path.len = (at + 4) as u8;
        path.depth += 1;
        path
    }

    /// Number of segments below the root.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth as usize
    }

    /// Segment `index`, counted from the root.
    #[must_use]
    pub const fn segment(&self, index: usize) -> [u8; 4] {
        assert!(
            index < self.depth as usize,
            "ACPI path segment out of range"
        );
        let at = 1 + index * 5;
        [
            self.text[at],
            self.text[at + 1],
            self.text[at + 2],
            self.text[at + 3],
        ]
    }

    /// The ASL spelling, e.g. `\_SB_.PCI0.LPCB`. Build the path itself in a
    /// const context (a `const` item or `const { }` block): `child` is only
    /// free when the compiler evaluates it.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        let (text, _) = self.text.split_at(self.len as usize);
        // SAFETY: `ROOT` and `child` write only `\\`, `.` and NameSeg bytes
        // that `name_seg` checked to be ASCII.
        unsafe { core::str::from_utf8_unchecked(text) }
    }

    /// Bytes of the AML NameString of a `depth`-segment absolute path.
    #[doc(hidden)]
    #[must_use]
    pub const fn name_string_len(depth: usize) -> usize {
        match depth {
            0 => 2,
            1 => 5,
            2 => 10,
            n => 3 + 4 * n,
        }
    }
}

/// `name` as a four-byte NameSeg: a letter or `_`, then letters, digits or
/// `_`, padded with `_`.
#[must_use]
pub const fn name_seg(name: &str) -> [u8; 4] {
    let bytes = name.as_bytes();
    assert!(
        !bytes.is_empty() && bytes.len() <= 4,
        "ACPI names have one to four characters"
    );
    let mut segment = [b'_'; 4];
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let valid =
            byte == b'_' || byte.is_ascii_uppercase() || (index > 0 && byte.is_ascii_digit());
        assert!(valid, "invalid ACPI NameSeg character");
        segment[index] = byte;
        index += 1;
    }
    segment
}

/// The devices of a PC chipset that boards, ECs and Super I/Os attach to.
/// The platform derives them from the names its chipset drivers give their
/// devices, so attached drivers never spell out the topology.
pub trait PcNamespace {
    /// The LPC/ISA bridge, parent of the EC and the Super I/O.
    const LPC: AcpiPath;
    /// The integrated graphics device.
    const IGD: AcpiPath;
}

#[cfg(test)]
mod tests {
    use super::*;

    const LPC: AcpiPath = AcpiPath::SB.child("PCI0").child("LPCB");

    #[test]
    fn paths_join_padded_segments() {
        assert_eq!(AcpiPath::ROOT.as_str(), "\\");
        assert_eq!(AcpiPath::SB.as_str(), "\\_SB_");
        assert_eq!(LPC.as_str(), "\\_SB_.PCI0.LPCB");
        assert_eq!(LPC.child("EC").as_str(), "\\_SB_.PCI0.LPCB.EC__");
        assert_eq!(LPC.depth(), 3);
        assert_eq!(&LPC.segment(2), b"LPCB");
        assert_eq!(name_seg("EC"), *b"EC__");
    }

    #[test]
    fn name_string_lengths_follow_the_prefix_rules() {
        assert_eq!(AcpiPath::name_string_len(0), 2); // \ NullName
        assert_eq!(AcpiPath::name_string_len(1), 5); // \ NameSeg
        assert_eq!(AcpiPath::name_string_len(2), 10); // \ DualNamePrefix
        assert_eq!(AcpiPath::name_string_len(3), 15); // \ MultiNamePrefix n
    }
}
