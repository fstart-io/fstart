// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::Result;
pub fn parse_spd_hex(text: &str) -> Result<[u8; 512]> {
    let digits: String = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    if digits.len() != 1024 || !digits.is_ascii() {
        return Err("SPD must contain exactly 512 hex bytes".into());
    }
    let mut result = [0; 512];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16)
            .map_err(|_| "non-hex SPD byte")?;
    }
    Ok(result)
}
