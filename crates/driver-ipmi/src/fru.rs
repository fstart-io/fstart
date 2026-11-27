// SPDX-License-Identifier: MIT OR Apache-2.0
//! IPMI Platform Management FRU Information Storage Definition (v1.0) parser
//! and SMBIOS identity overlay.

use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

use crate::Error;
use crate::kcs::{Kcs, Status};

/// Maximum length of a decoded FRU text field: IPMI FRU type/length encodes
/// at most 63 bytes, and BCD-plus expands 63 bytes to 126 characters.
const MAX_FIELD_LEN: usize = 126;

/// Maximum size of a single FRU info area (length byte `0xff` * 8 = 2040).
const MAX_AREA_BYTES: usize = 255 * 8;

/// Maximum valid SMBIOS Type 3 chassis type byte (DSP0134 Table 17).
const MAX_CHASSIS_TYPE: u8 = 0x24;

/// A bounded ASCII string decoded from an IPMI FRU area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FruString {
    buf: [u8; MAX_FIELD_LEN],
    len: u8,
}

impl FruString {
    pub const EMPTY: Self = Self {
        buf: [0; MAX_FIELD_LEN],
        len: 0,
    };

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push_ascii(&mut self, byte: u8) {
        if (0x20..=0x7e).contains(&byte) && usize::from(self.len) < MAX_FIELD_LEN {
            self.buf[usize::from(self.len)] = byte;
            self.len += 1;
        }
    }

    fn trim_end(&mut self) {
        while self.len > 0 && self.buf[usize::from(self.len - 1)].is_ascii_whitespace() {
            self.len -= 1;
        }
    }
}

/// Factory identity fields extracted from an IPMI FRU device and BMC GUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FruIdentity {
    pub chassis_type: Option<u8>,
    pub chassis_part_number: FruString,
    pub chassis_serial: FruString,
    pub board_manufacturer: FruString,
    pub board_product: FruString,
    pub board_serial: FruString,
    pub board_part_number: FruString,
    pub product_manufacturer: FruString,
    pub product_name: FruString,
    pub product_part_number: FruString,
    pub product_version: FruString,
    pub product_serial: FruString,
    pub product_asset_tag: FruString,
    /// System UUID in SMBIOS 2.6+ wire order, from `Get System GUID`.
    pub system_uuid: Option<[u8; 16]>,
}

impl FruIdentity {
    pub const EMPTY: Self = Self {
        chassis_type: None,
        chassis_part_number: FruString::EMPTY,
        chassis_serial: FruString::EMPTY,
        board_manufacturer: FruString::EMPTY,
        board_product: FruString::EMPTY,
        board_serial: FruString::EMPTY,
        board_part_number: FruString::EMPTY,
        product_manufacturer: FruString::EMPTY,
        product_name: FruString::EMPTY,
        product_part_number: FruString::EMPTY,
        product_version: FruString::EMPTY,
        product_serial: FruString::EMPTY,
        product_asset_tag: FruString::EMPTY,
        system_uuid: None,
    };

    /// Parse an IPMI FRU v1.0 image (common header plus chassis, board, and
    /// product info areas) held in memory, as [`Self::read_system`] reads
    /// one over KCS.
    #[cfg(test)]
    fn parse(raw: &[u8]) -> Result<Self, Error> {
        let size = u16::try_from(raw.len()).unwrap_or(u16::MAX);
        Self::read_areas(size, |offset, out| {
            let data = raw.get(usize::from(offset)..).ok_or(Error::Short)?;
            let n = out.len().min(data.len());
            out[..n].copy_from_slice(&data[..n]);
            Ok(n)
        })
    }

    /// Read the system's FRU and the BMC's system GUID over KCS: the first
    /// logical FRU device, from 0 up, with a board area naming
    /// `board_product` (any board area if `None`).
    ///
    /// Most BMCs have the baseboard at FRU 0. OpenBMC numbers its FRUs by
    /// a hash of their D-Bus names: the S8040's has no FRU 0 (`0xCB`), its
    /// baseboard is at 74, and its power supplies, product areas only, are
    /// at 72 and 101. The board product keeps the scan off any other FRU
    /// with a board area, such as a riser's or a NIC's.
    pub fn read_system<D, P, S>(kcs: &mut Kcs<D, P, S>, board_product: Option<&str>) -> Option<Self>
    where
        D: DelayNs,
        P: Readable<T = u8> + Writeable<T = u8>,
        S: Readable<T = u8, R = Status::Register> + Writeable<T = u8, R = Status::Register>,
    {
        let id = Self::find_system(board_product, |fru_id| {
            let (size, word_access) = kcs.fru_inventory_area_info(fru_id)?;
            Self::read_areas(size, |offset, out| {
                kcs.read_fru_range(fru_id, offset, word_access, out)
            })
        })
        .ok()?;
        let uuid = kcs.system_guid().ok().and_then(ipmi_guid_to_smbios_uuid);
        match (id, uuid) {
            (Some(mut fru), uuid) => {
                fru.system_uuid = uuid;
                Some(fru)
            }
            (None, Some(uuid)) => Some(Self {
                system_uuid: Some(uuid),
                ..Self::EMPTY
            }),
            (None, None) => None,
        }
    }

    /// The first FRU of IDs 0..=254 that `read` returns with a board area
    /// naming `board_product` (any, if `None`). `Err` if the BMC does not
    /// answer (one timeout, not 255); `Ok(None)` if no FRU matches.
    fn find_system(
        board_product: Option<&str>,
        mut read: impl FnMut(u8) -> Result<Self, Error>,
    ) -> Result<Option<Self>, ()> {
        for fru_id in 0..=0xfe {
            match read(fru_id) {
                Ok(fru)
                    if fru.has_board()
                        && board_product.is_none_or(|p| fru.board_product.as_str() == p) =>
                {
                    return Ok(Some(fru));
                }
                Err(Error::Timeout | Error::Stuck) => return Err(()),
                Ok(_) | Err(_) => {}
            }
        }
        Ok(None)
    }

    /// A board area with an identity in it.
    fn has_board(&self) -> bool {
        !(self.board_manufacturer.is_empty()
            && self.board_product.is_empty()
            && self.board_serial.is_empty())
    }

    /// The areas of a FRU of `size` bytes, which `read(offset, out)` reads.
    ///
    /// Reads the 8-byte common header first, then fetches each present area
    /// (chassis, board, product) at its exact offset and length, so FRUs
    /// larger than one read are never silently truncated. An area that is
    /// out of bounds or fails its checksum is skipped.
    fn read_areas(
        size: u16,
        mut read: impl FnMut(u16, &mut [u8]) -> Result<usize, Error>,
    ) -> Result<Self, Error> {
        use zerocopy::FromBytes;

        let mut raw = [0u8; core::mem::size_of::<FruCommonHeader>()];
        if size < raw.len() as u16 || read(0, &mut raw)? < raw.len() {
            return Err(Error::Short);
        }
        let header = FruCommonHeader::read_from_bytes(&raw).map_err(|_| Error::Short)?;
        if (header.format_version & 0x0f) != 1 || !checksum_ok(&raw) {
            return Err(Error::Short);
        }

        let mut id = Self::EMPTY;
        let mut buf = [0u8; MAX_AREA_BYTES];
        if let Some(area) = read_area(&mut read, size, header.chassis_info_offset, &mut buf)? {
            id.parse_chassis(area);
        }
        if let Some(area) = read_area(&mut read, size, header.board_info_offset, &mut buf)? {
            id.parse_board(area);
        }
        if let Some(area) = read_area(&mut read, size, header.product_info_offset, &mut buf)? {
            id.parse_product(area);
        }
        Ok(id)
    }

    /// Overlay non-empty FRU fields and system UUID onto a board's static
    /// SMBIOS identity.
    #[cfg(feature = "acpi")]
    #[must_use]
    pub fn apply_to<'a>(
        &'a self,
        base: fstart_acpi::smbios::SmbiosIdentity<'a>,
    ) -> fstart_acpi::smbios::SmbiosIdentity<'a> {
        let mut identity = base;
        if !self.product_version.is_empty() {
            identity.sys_version = self.product_version.as_str();
        }
        if !self.product_serial.is_empty() {
            identity.sys_serial = Some(self.product_serial.as_str());
        }
        if let Some(uuid) = self.system_uuid {
            identity.sys_uuid = Some(uuid);
        }
        if !self.board_serial.is_empty() {
            identity.bb_serial = Some(self.board_serial.as_str());
        }
        // Only override a board's static chassis type with a specific FRU
        // chassis type (0x03..=0x24), or when the board left it Unknown (0x02).
        if let Some(kind) = self.chassis_type
            && (kind >= 0x03 || identity.chassis_type == 0x02)
        {
            identity.chassis_type = kind;
        }
        if identity.chassis_manufacturer.is_empty() {
            if !self.product_manufacturer.is_empty() {
                identity.chassis_manufacturer = self.product_manufacturer.as_str();
            } else if !self.board_manufacturer.is_empty() {
                identity.chassis_manufacturer = self.board_manufacturer.as_str();
            }
        }
        if !self.chassis_serial.is_empty() {
            identity.chassis_serial = Some(self.chassis_serial.as_str());
        }
        identity
    }

    fn parse_chassis(&mut self, area: &[u8]) {
        use zerocopy::FromBytes;

        let Ok((prefix, rest)) = ChassisAreaPrefix::ref_from_prefix(area) else {
            return;
        };
        let kind = prefix.chassis_type & 0x7f;
        if (1..=MAX_CHASSIS_TYPE).contains(&kind) {
            self.chassis_type = Some(kind);
        }
        let mut cursor = rest.get(..rest.len().saturating_sub(1)).unwrap_or(&[]);
        if let Some(s) = next_field(&mut cursor, true) {
            self.chassis_part_number = s;
        }
        if let Some(s) = next_field(&mut cursor, true) {
            self.chassis_serial = s;
        }
    }

    fn parse_board(&mut self, area: &[u8]) {
        use zerocopy::FromBytes;

        let Ok((prefix, rest)) = BoardAreaPrefix::ref_from_prefix(area) else {
            return;
        };
        let english = is_english(prefix.language);
        let mut cursor = rest.get(..rest.len().saturating_sub(1)).unwrap_or(&[]);
        if let Some(s) = next_field(&mut cursor, english) {
            self.board_manufacturer = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.board_product = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.board_serial = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.board_part_number = s;
        }
    }

    fn parse_product(&mut self, area: &[u8]) {
        use zerocopy::FromBytes;

        let Ok((prefix, rest)) = ProductAreaPrefix::ref_from_prefix(area) else {
            return;
        };
        let english = is_english(prefix.language);
        let mut cursor = rest.get(..rest.len().saturating_sub(1)).unwrap_or(&[]);
        if let Some(s) = next_field(&mut cursor, english) {
            self.product_manufacturer = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.product_name = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.product_part_number = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.product_version = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.product_serial = s;
        }
        if let Some(s) = next_field(&mut cursor, english) {
            self.product_asset_tag = s;
        }
    }
}

#[repr(C)]
#[derive(
    Clone,
    Copy,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
    zerocopy::Unaligned,
)]
struct FruCommonHeader {
    format_version: u8,
    internal_use_offset: u8,
    chassis_info_offset: u8,
    board_info_offset: u8,
    product_info_offset: u8,
    multirecord_offset: u8,
    pad: u8,
    checksum: u8,
}

#[repr(C)]
#[derive(
    Clone,
    Copy,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
    zerocopy::Unaligned,
)]
struct FruAreaHeader {
    format_version: u8,
    length_blocks: u8,
}

#[repr(C)]
#[derive(
    Clone,
    Copy,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
    zerocopy::Unaligned,
)]
struct ChassisAreaPrefix {
    header: FruAreaHeader,
    chassis_type: u8,
}

#[repr(C)]
#[derive(
    Clone,
    Copy,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
    zerocopy::Unaligned,
)]
struct BoardAreaPrefix {
    header: FruAreaHeader,
    language: u8,
    mfg_date: [u8; 3],
}

#[repr(C)]
#[derive(
    Clone,
    Copy,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
    zerocopy::Unaligned,
)]
struct ProductAreaPrefix {
    header: FruAreaHeader,
    language: u8,
}

tock_registers::register_bitfields![u8,
    /// FRU Type/Length byte (IPMI FRU spec §13, 8 bits).
    TypeLength [
        LENGTH OFFSET(0) NUMBITS(6) [],
        KIND OFFSET(6) NUMBITS(2) []
    ]
];

/// The SMBIOS 2.6+ UUID (DSP0134 §7.2.1) of a `Get System GUID` (IPMI
/// 2.0 §22.14) payload: its bytes as they come, which BMCs send in SMBIOS
/// wire order (time_low first, little-endian), not the reversed layout of
/// IPMI's original Table 20-10. The S8040's OpenBMC answers
/// `40 80 00 00 43 59 …`, and its vendor firmware publishes
/// 00008040-5943-464d-3242-4e3434303048 from it, the UUID the system must
/// keep across firmware; `ipmitool mc guid` decodes it the same way.
///
/// Rejects all-zero ("not present") and all-0xFF ("not settable") placeholders.
pub fn ipmi_guid_to_smbios_uuid(guid: [u8; 16]) -> Option<[u8; 16]> {
    if guid.iter().all(|&b| b == 0x00) || guid.iter().all(|&b| b == 0xff) {
        return None;
    }
    Some(guid)
}

fn is_english(lang: u8) -> bool {
    lang == 0 || lang == 25
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0
}

/// The info area at `offset_blocks` (8-byte units) of a FRU of `size`
/// bytes, read into `buf`; `None` if absent, out of bounds or corrupt.
fn read_area<'a>(
    read: &mut impl FnMut(u16, &mut [u8]) -> Result<usize, Error>,
    size: u16,
    offset_blocks: u8,
    buf: &'a mut [u8; MAX_AREA_BYTES],
) -> Result<Option<&'a [u8]>, Error> {
    if offset_blocks == 0 {
        return Ok(None);
    }
    let start = u16::from(offset_blocks) * 8;
    if usize::from(start) + 2 > usize::from(size) {
        return Ok(None);
    }
    let mut raw_hdr = [0u8; core::mem::size_of::<FruAreaHeader>()];
    if read(start, &mut raw_hdr)? < raw_hdr.len() {
        return Ok(None);
    }
    let Ok(hdr) = zerocopy::FromBytes::read_from_bytes(&raw_hdr) else {
        return Ok(None);
    };
    let hdr: FruAreaHeader = hdr;
    if (hdr.format_version & 0x0f) != 1 || hdr.length_blocks == 0 {
        return Ok(None);
    }
    let len = usize::from(hdr.length_blocks) * 8;
    if usize::from(start) + len > usize::from(size) {
        return Ok(None);
    }
    let area = &mut buf[..len];
    if read(start, area)? < len || !checksum_ok(area) {
        return Ok(None);
    }
    Ok(Some(area))
}

fn next_field(cursor: &mut &[u8], english: bool) -> Option<FruString> {
    let (&tl, rest) = cursor.split_first()?;
    // 0xC1 marks end of fields in an area.
    if tl == 0xc1 {
        *cursor = &[];
        return None;
    }
    let kind = TypeLength::KIND.read(tl);
    let len = usize::from(TypeLength::LENGTH.read(tl));
    let data = rest.get(..len)?;
    *cursor = &rest[len..];

    let mut out = FruString::EMPTY;
    match kind {
        0b11 => {
            // In English areas (language 0 or 25), type 11b is 8-bit ASCII +
            // Latin-1; in other languages it is 2-byte UCS-2 which we do not
            // silently truncate.
            if english {
                for &b in data {
                    if b == 0 {
                        break;
                    }
                    out.push_ascii(b);
                }
            }
        }
        0b10 => {
            // 6-bit packed ASCII: 3 bytes -> 4 characters (+ 0x20)
            for chunk in data.chunks(3) {
                let b0 = chunk[0];
                let b1 = chunk.get(1).copied().unwrap_or(0);
                let b2 = chunk.get(2).copied().unwrap_or(0);
                out.push_ascii((b0 & 0x3f) + 0x20);
                if chunk.len() >= 2 {
                    out.push_ascii((((b0 >> 6) | (b1 << 2)) & 0x3f) + 0x20);
                }
                if chunk.len() >= 3 {
                    out.push_ascii((((b1 >> 4) | (b2 << 4)) & 0x3f) + 0x20);
                    out.push_ascii(((b2 >> 2) & 0x3f) + 0x20);
                }
            }
        }
        0b01 => {
            // BCD plus (FRU spec §13): two 4-bit characters per byte.
            const BCD: &[u8; 16] = b"0123456789 -.:,_";
            for &b in data {
                out.push_ascii(BCD[usize::from((b >> 4) & 0x0f)]);
                out.push_ascii(BCD[usize::from(b & 0x0f)]);
            }
        }
        _ => {}
    }
    out.trim_end();
    Some(out)
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::{vec, vec::Vec};

    fn with_board(product: &str, serial: &str) -> FruIdentity {
        let mut fru = FruIdentity::EMPTY;
        product
            .bytes()
            .for_each(|b| fru.board_product.push_ascii(b));
        serial.bytes().for_each(|b| fru.board_serial.push_ascii(b));
        fru
    }

    /// The S8040's OpenBMC: no FRU 0, a supply (product area only) at 72,
    /// the baseboard at 74. The scan skips both to the baseboard, and a
    /// riser's board area before it when the board product is given.
    #[test]
    fn find_system_skips_absent_board_less_and_foreign_frus() {
        let frus = |id| match id {
            30 => Ok(with_board("RISER", "R1")),
            72 => {
                let mut psu = FruIdentity::EMPTY;
                b"A00233500002000AGBFSM009"
                    .iter()
                    .for_each(|&b| psu.product_serial.push_ascii(b));
                Ok(psu)
            }
            74 => Ok(with_board("S8040GM2NE", "CYMF2BN4400H")),
            _ => Err(Error::Completion(0xcb)),
        };
        let mut asked = Vec::new();
        let found = FruIdentity::find_system(Some("S8040GM2NE"), |id| {
            asked.push(id);
            frus(id)
        });
        assert_eq!(
            found.unwrap().unwrap().board_serial.as_str(),
            "CYMF2BN4400H"
        );
        assert_eq!(asked.last(), Some(&74));
        // Without a product, the first board area wins.
        let any = FruIdentity::find_system(None, frus);
        assert_eq!(any.unwrap().unwrap().board_serial.as_str(), "R1");
    }

    /// FRU 0 with a board area is taken without a scan; a BMC that times
    /// out costs one query; none with a matching board area is `Ok(None)`.
    #[test]
    fn find_system_prefers_fru0_and_stops_on_timeout() {
        let mut asked = 0;
        let fru0 = FruIdentity::find_system(Some("S8040GM2NE"), |_| {
            asked += 1;
            Ok(with_board("S8040GM2NE", "B0"))
        });
        assert_eq!(
            (fru0.unwrap().unwrap().board_serial.as_str(), asked),
            ("B0", 1)
        );

        let mut asked = 0;
        let absent = FruIdentity::find_system(None, |_| {
            asked += 1;
            Err(Error::Timeout)
        });
        assert_eq!((absent, asked), (Err(()), 1));

        let none = FruIdentity::find_system(None, |_| Err(Error::Completion(0xcb)));
        assert_eq!(none, Ok(None));
        let other = FruIdentity::find_system(Some("S8040GM2NE"), |_| Ok(with_board("X", "1")));
        assert_eq!(other, Ok(None));
    }

    fn finish_area(mut area: Vec<u8>) -> Vec<u8> {
        area.push(0xc1); // end of fields
        // Pad to 8-byte multiple leaving 1 byte for checksum.
        let total = (area.len() + 1).next_multiple_of(8);
        area.resize(total, 0);
        area[1] = (total / 8) as u8;
        let sum = area[..total - 1]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        area[total - 1] = sum.wrapping_neg();
        area
    }

    fn push_ascii(buf: &mut Vec<u8>, s: &str) {
        buf.push(0xc0 | (s.len() as u8));
        buf.extend_from_slice(s.as_bytes());
    }

    fn build_fru(chassis: &[u8], board: &[u8], product: &[u8], internal_pad_blocks: u8) -> Vec<u8> {
        let mut raw = vec![0u8; 8 + usize::from(internal_pad_blocks) * 8];
        let mut off = 1 + internal_pad_blocks;
        let chassis_off = if chassis.is_empty() {
            0
        } else {
            let o = off;
            raw.extend_from_slice(chassis);
            off += (chassis.len() / 8) as u8;
            o
        };
        let board_off = if board.is_empty() {
            0
        } else {
            let o = off;
            raw.extend_from_slice(board);
            off += (board.len() / 8) as u8;
            o
        };
        let product_off = if product.is_empty() {
            0
        } else {
            let o = off;
            raw.extend_from_slice(product);
            o
        };
        let mut header = [1u8, 0, chassis_off, board_off, product_off, 0, 0, 0];
        header[7] = header[..7]
            .iter()
            .fold(0u8, |a, &b| a.wrapping_add(b))
            .wrapping_neg();
        raw[..8].copy_from_slice(&header);
        raw
    }

    #[test]
    fn parses_chassis_board_and_product_areas() {
        let mut chassis = vec![1, 0, 0x17]; // rack mount
        push_ascii(&mut chassis, "CHASSIS-PN");
        push_ascii(&mut chassis, "CHASSIS-SN-1 ");
        let chassis = finish_area(chassis);

        let mut board = vec![1, 0, 25, 0, 0, 0];
        push_ascii(&mut board, "TYAN");
        push_ascii(&mut board, "S8040GM2NE");
        push_ascii(&mut board, "CYMF2BN4400H");
        push_ascii(&mut board, "S8040-PN");
        let board = finish_area(board);

        let mut product = vec![1, 0, 25];
        push_ascii(&mut product, "TYAN");
        push_ascii(&mut product, "S8040GM2NE");
        push_ascii(&mut product, "S8040-MODEL");
        push_ascii(&mut product, "Rev 1.0");
        push_ascii(&mut product, "USE2600043EBA00D");
        push_ascii(&mut product, "ASSET-9");
        let product = finish_area(product);

        let raw = build_fru(&chassis, &board, &product, 0);
        let fru = FruIdentity::parse(&raw).unwrap();
        assert_eq!(fru.chassis_type, Some(0x17));
        assert_eq!(fru.chassis_part_number.as_str(), "CHASSIS-PN");
        assert_eq!(fru.chassis_serial.as_str(), "CHASSIS-SN-1");
        assert_eq!(fru.board_manufacturer.as_str(), "TYAN");
        assert_eq!(fru.board_product.as_str(), "S8040GM2NE");
        assert_eq!(fru.board_serial.as_str(), "CYMF2BN4400H");
        assert_eq!(fru.product_version.as_str(), "Rev 1.0");
        assert_eq!(fru.product_serial.as_str(), "USE2600043EBA00D");
        assert_eq!(fru.product_asset_tag.as_str(), "ASSET-9");
    }

    #[test]
    fn decodes_6bit_packed_ascii_and_bcd_plus_and_rejects_bad_checksums() {
        // Pack "TYAN" in 6-bit ASCII (each char - 0x20):
        // 'T'=0x34, 'Y'=0x39, 'A'=0x21, 'N'=0x2e
        let c = [0x34u8, 0x39, 0x21, 0x2e];
        let packed = [
            c[0] | ((c[1] & 0x03) << 6),
            (c[1] >> 2) | ((c[2] & 0x0f) << 4),
            (c[2] >> 4) | (c[3] << 2),
        ];
        // BCD-plus "12-3": 0x12, 0xb3 (0xb = '-')
        let mut board = vec![1, 0, 25, 0, 0, 0];
        board.push(0x80 | 3); // 6-bit packed ASCII, 3 bytes
        board.extend_from_slice(&packed);
        board.push(0x40 | 2); // BCD-plus, 2 bytes
        board.extend_from_slice(&[0x12, 0xb3]);
        let board = finish_area(board);

        let mut raw = build_fru(&[], &board, &[], 0);
        let fru = FruIdentity::parse(&raw).unwrap();
        assert_eq!(fru.board_manufacturer.as_str(), "TYAN");
        assert_eq!(fru.board_product.as_str(), "12-3");

        // Corrupting common header checksum rejects the image.
        raw[7] ^= 1;
        assert!(FruIdentity::parse(&raw).is_err());
    }

    #[test]
    fn system_guid_is_the_smbios_uuid_and_rejects_placeholders() {
        assert_eq!(ipmi_guid_to_smbios_uuid([0x00; 16]), None);
        assert_eq!(ipmi_guid_to_smbios_uuid([0xff; 16]), None);
        // The S8040's BMC; its vendor firmware's SMBIOS UUID is
        // 00008040-5943-464d-3242-4e3434303048, which dmidecode prints from
        // exactly these wire bytes.
        let guid = [
            0x40, 0x80, 0x00, 0x00, 0x43, 0x59, 0x4d, 0x46, 0x32, 0x42, 0x4e, 0x34, 0x34, 0x30,
            0x30, 0x48,
        ];
        assert_eq!(ipmi_guid_to_smbios_uuid(guid), Some(guid));
    }

    #[cfg(feature = "acpi")]
    #[test]
    fn apply_to_populates_smbios_identity_and_preserves_specific_chassis_type() {
        let mut fru = FruIdentity::EMPTY;
        fru.chassis_type = Some(0x02); // Unknown in FRU must not downgrade static 0x17
        for &b in b"CHASSIS-1" {
            fru.chassis_serial.push_ascii(b);
        }
        for &b in b"CYMF2BN4400H" {
            fru.board_serial.push_ascii(b);
        }
        for &b in b"USE2600043EBA00D" {
            fru.product_serial.push_ascii(b);
        }
        fru.system_uuid = Some([1; 16]);

        let mut base = fstart_acpi::smbios::SmbiosIdentity::new("TYAN", "S8040GM2NE");
        base.chassis_type = 0x17;
        let id = fru.apply_to(base);
        assert_eq!(id.sys_serial, Some("USE2600043EBA00D"));
        assert_eq!(id.bb_serial, Some("CYMF2BN4400H"));
        assert_eq!(id.chassis_serial, Some("CHASSIS-1"));
        assert_eq!(id.chassis_type, 0x17);
        assert_eq!(id.sys_uuid, Some([1; 16]));
    }
}
