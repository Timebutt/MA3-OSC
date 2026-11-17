//! Behringer X-Touch (Mackie Control mode) display helpers: scribble-strip text,
//! scribble-strip colors and the 7-segment assignment/timecode display.

/// Mackie Control device id of the X-Touch main unit.
pub const MC_DEVICE_ID: u8 = 0x14;
/// Mackie Control device id of the X-Touch Extender.
pub const MC_EXTENDER_ID: u8 = 0x15;
pub const STRIP_CHARS: usize = 7;

/// The 8 colors the X-Touch scribble strips can show (index = color code).
const PALETTE: [(u8, [f64; 3]); 7] = [
    (1, [255.0, 0.0, 0.0]),     // red
    (2, [0.0, 255.0, 0.0]),     // green
    (3, [255.0, 255.0, 0.0]),   // yellow
    (4, [0.0, 0.0, 255.0]),     // blue
    (5, [255.0, 0.0, 255.0]),   // magenta
    (6, [0.0, 255.0, 255.0]),   // cyan
    (7, [255.0, 255.0, 255.0]), // white
];
pub const COLOR_OFF: u8 = 0;
pub const COLOR_RED: u8 = 1;
pub const COLOR_WHITE: u8 = 7;

/// Map an MA appearance color string "r;g;b;a" (0-255 each) to the nearest strip color.
/// Unparseable input maps to white so the strip stays readable.
pub fn nearest_color(rgba: &str) -> u8 {
    let parts: Vec<f64> = rgba.split([';', ',']).map(|p| p.trim().parse::<f64>().unwrap_or(f64::NAN)).collect();
    if parts.len() < 3 || parts[..3].iter().any(|v| v.is_nan()) {
        return COLOR_WHITE;
    }
    let (r, g, b) = (parts[0], parts[1], parts[2]);
    let a = parts.get(3).copied().filter(|v| !v.is_nan()).unwrap_or(255.0);
    if (r == 0.0 && g == 0.0 && b == 0.0) || a == 0.0 {
        return COLOR_OFF;
    }
    PALETTE
        .iter()
        .min_by(|x, y| dist(x.1, [r, g, b]).total_cmp(&dist(y.1, [r, g, b])))
        .map(|p| p.0)
        .unwrap_or(COLOR_WHITE)
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Convert arbitrary text into exactly 7 printable 7-bit ASCII bytes.
/// Accented Latin letters lose their accent, anything else becomes '?'.
/// (Bytes above 0x7F are illegal inside SysEx and used to corrupt the message.)
pub fn strip_text(s: &str) -> [u8; STRIP_CHARS] {
    let mut out = [b' '; STRIP_CHARS];
    let mut i = 0;
    for ch in s.chars() {
        if i == STRIP_CHARS {
            break;
        }
        let ascii_buf = [ch as u8];
        let mapped: &[u8] = match ch {
            ' '..='~' => &ascii_buf,
            c if c.is_ascii() => b" ", // control characters
            c => fold(c).as_bytes(),
        };
        for &b in mapped {
            if i == STRIP_CHARS {
                break;
            }
            out[i] = b;
            i += 1;
        }
    }
    out
}

/// ASCII replacement for a non-ASCII character.
fn fold(c: char) -> &'static str {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => "A",
        'æ' => "ae",
        'Æ' => "AE",
        'ç' => "c",
        'Ç' => "C",
        'è' | 'é' | 'ê' | 'ë' => "e",
        'È' | 'É' | 'Ê' | 'Ë' => "E",
        'ì' | 'í' | 'î' | 'ï' => "i",
        'Ì' | 'Í' | 'Î' | 'Ï' => "I",
        'ñ' => "n",
        'Ñ' => "N",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => "O",
        'ù' | 'ú' | 'û' | 'ü' => "u",
        'Ù' | 'Ú' | 'Û' | 'Ü' => "U",
        'ý' | 'ÿ' => "y",
        'Ý' => "Y",
        'ß' => "ss",
        '–' | '—' => "-",
        '‘' | '’' => "'",
        '“' | '”' => "\"",
        _ => "?",
    }
}

/// SysEx writing 7 characters at an LCD offset (row 0: strip*7, row 1: 56 + strip*7).
pub fn lcd_text_sysex(device_id: u8, offset: u8, text: &[u8; STRIP_CHARS]) -> Vec<u8> {
    let mut v = vec![0xF0, 0x00, 0x00, 0x66, device_id, 0x12, offset & 0x7F];
    v.extend_from_slice(text);
    v.push(0xF7);
    v
}

/// SysEx setting all 8 strip colors at once (Behringer X-Touch extension).
pub fn lcd_colors_sysex(device_id: u8, colors: &[u8; 8]) -> Vec<u8> {
    let mut v = vec![0xF0, 0x00, 0x00, 0x66, device_id, 0x72];
    v.extend(colors.iter().map(|c| c & 0x07));
    v.push(0xF7);
    v
}

pub fn lcd_offset(strip: u8, row: u8) -> u8 {
    row * 56 + strip * STRIP_CHARS as u8
}

/// 7-segment display: segment index 1..=12 is CC (75 - segment) on channel 1.
pub fn segment_cc(segment: u8) -> u8 {
    75 - segment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_sanitised_and_padded() {
        assert_eq!(&strip_text("Intro"), b"Intro  ");
        assert_eq!(&strip_text("Café Ölé"), b"Cafe Ol");
        assert_eq!(&strip_text("日本"), b"??     ");
        assert_eq!(&strip_text("a\tb"), b"a b    ");
        assert!(strip_text("ßßßß").iter().all(|b| *b < 0x80));
    }

    #[test]
    fn colors() {
        assert_eq!(nearest_color("255;0;0;255"), COLOR_RED);
        assert_eq!(nearest_color("0;0;0;255"), COLOR_OFF);
        assert_eq!(nearest_color("255;255;255;0"), COLOR_OFF);
        assert_eq!(nearest_color("250;240;10;255"), 3);
        assert_eq!(nearest_color("garbage"), COLOR_WHITE);
        assert_eq!(nearest_color("255,0,255,255"), 5);
    }

    #[test]
    fn sysex_shapes() {
        let s = lcd_text_sysex(MC_DEVICE_ID, lcd_offset(1, 1), &strip_text("Cue 1"));
        assert_eq!(&s[..7], &[0xF0, 0, 0, 0x66, 0x14, 0x12, 63]);
        assert_eq!(*s.last().unwrap(), 0xF7);
        assert!(s[1..s.len() - 1].iter().all(|b| *b < 0x80));
    }
}
