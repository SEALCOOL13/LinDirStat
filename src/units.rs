//! Size formatting: base-2 / base-10 and a selectable display unit.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Base {
    Binary,  // 1024: KiB, MiB, ...
    Decimal, // 1000: KB, MB, ...
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Unit {
    Auto,
    Byte,
    Bit,
    K,
    M,
    G,
    T,
    P,
}

impl Unit {
    pub const ALL: [Unit; 8] = [Unit::Auto, Unit::Byte, Unit::Bit, Unit::K, Unit::M, Unit::G, Unit::T, Unit::P];

    pub fn label(self, base: Base) -> &'static str {
        let bin = base == Base::Binary;
        match self {
            Unit::Auto => "Auto",
            Unit::Byte => "Bytes (B)",
            Unit::Bit => "Bits (b)",
            Unit::K => if bin { "KiB" } else { "KB" },
            Unit::M => if bin { "MiB" } else { "MB" },
            Unit::G => if bin { "GiB" } else { "GB" },
            Unit::T => if bin { "TiB" } else { "TB" },
            Unit::P => if bin { "PiB" } else { "PB" },
        }
    }
}

fn suffix(idx: usize, base: Base) -> &'static str {
    const BIN: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    const DEC: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if base == Base::Binary { BIN[idx] } else { DEC[idx] }
}

/// 1234567 -> "1,234,567"
pub fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn group_decimal(s: &str) -> String {
    match s.split_once('.') {
        Some((i, f)) => format!("{}.{}", fmt_int(i.parse().unwrap_or(0)), f),
        None => fmt_int(s.parse().unwrap_or(0)),
    }
}

pub fn fmt_size(bytes: u64, base: Base, unit: Unit) -> String {
    let div = if base == Base::Binary { 1024.0f64 } else { 1000.0f64 };
    match unit {
        Unit::Auto => {
            let mut v = bytes as f64;
            let mut idx = 0;
            while v >= div && idx < 5 {
                v /= div;
                idx += 1;
            }
            if idx == 0 {
                format!("{} B", bytes)
            } else {
                format!("{:.1} {}", v, suffix(idx, base))
            }
        }
        Unit::Byte => format!("{} B", fmt_int(bytes)),
        Unit::Bit => format!("{} b", fmt_int(bytes.saturating_mul(8))),
        k => {
            let idx = match k {
                Unit::K => 1,
                Unit::M => 2,
                Unit::G => 3,
                Unit::T => 4,
                _ => 5,
            };
            let v = bytes as f64 / div.powi(idx as i32);
            format!("{} {}", group_decimal(&format!("{:.2}", v)), suffix(idx, base))
        }
    }
}

pub fn fmt_time(t: i64) -> String {
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = t as libc::time_t;
        if libc::localtime_r(&tt, &mut tm).is_null() {
            return String::new();
        }
        let mut buf = [0 as libc::c_char; 32];
        let n = libc::strftime(buf.as_mut_ptr(), 32, b"%Y-%m-%d %H:%M\0".as_ptr() as *const libc::c_char, &tm);
        std::slice::from_raw_parts(buf.as_ptr() as *const u8, n).iter().map(|&b| b as char).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats() {
        assert_eq!(fmt_size(1536, Base::Binary, Unit::Auto), "1.5 KiB");
        assert_eq!(fmt_size(1500, Base::Decimal, Unit::Auto), "1.5 KB");
        assert_eq!(fmt_size(999, Base::Decimal, Unit::Auto), "999 B");
        assert_eq!(fmt_size(1_048_576, Base::Binary, Unit::M), "1.00 MiB");
        assert_eq!(fmt_size(1_048_576, Base::Decimal, Unit::M), "1.05 MB");
        assert_eq!(fmt_size(2_500_000_000, Base::Decimal, Unit::Byte), "2,500,000,000 B");
        assert_eq!(fmt_size(10, Base::Binary, Unit::Bit), "80 b");
        assert_eq!(fmt_size(5_000_000_000_000, Base::Decimal, Unit::G), "5,000.00 GB");
    }
}
