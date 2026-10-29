use std::ffi::c_char;

use vst3::Steinberg::{TUID, Vst::TChar};

/// Reads a fixed-size, NUL-terminated C string field.
pub fn cstr_field(field: &[c_char]) -> String {
    let bytes: Vec<u8> = field
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Reads a fixed-size, NUL-terminated UTF-16 field.
pub fn wstr_field(field: &[TChar]) -> String {
    let units: Vec<u16> = field
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u16)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Writes `src` into a fixed-size UTF-16 field, truncating and always NUL-terminating.
pub fn copy_wstr(src: &str, dst: &mut [TChar]) {
    let Some(max) = dst.len().checked_sub(1) else {
        return;
    };
    let mut n = 0;
    for unit in src.encode_utf16().take(max) {
        dst[n] = unit as TChar;
        n += 1;
    }
    dst[n] = 0;
}

/// Formats a class id as 32 upper-case hex digits, in memory order. This matches the
/// `moduleinfo.json` "CID" spelling on Linux.
pub fn tuid_to_hex(tuid: &TUID) -> String {
    tuid.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

pub fn tuid_from_hex(s: &str) -> Option<TUID> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()? as c_char;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuid_hex_round_trip() {
        let t: TUID = std::array::from_fn(|i| (i as u8).wrapping_mul(37) as c_char);
        let hex = tuid_to_hex(&t);
        assert_eq!(hex.len(), 32);
        assert_eq!(tuid_from_hex(&hex), Some(t));
        assert_eq!(tuid_from_hex("zz"), None);
    }

    #[test]
    fn wide_strings_truncate_and_terminate() {
        let mut buf = [1 as TChar; 4];
        copy_wstr("Gain", &mut buf);
        assert_eq!(wstr_field(&buf), "Gai");
        assert_eq!(buf[3], 0);
    }
}
