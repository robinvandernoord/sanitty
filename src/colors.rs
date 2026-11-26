//! Colour values in style files (`#3366ff`, `rgb(...)`, `hsl(...)`), so the
//! editor can show a swatch in front of each, and SCSS/Less variables, which
//! the CSS grammar (used for those files) leaves uncoloured.

use std::path::Path;

use crate::util::to_u32;
use karet_core::LineCol;

/// Files whose colour values get swatches.
pub fn wants_swatches(path: &Path) -> bool {
    crate::util::is_style_file(path)
}

/// Whether `chr` continues a name, so a colour cannot start or end next to it.
fn is_name_char(chr: char) -> bool {
    chr.is_alphanumeric() || chr == '-' || chr == '_'
}

/// A colour channel from a float, clamped to `0..=255` and rounded.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0..=255 and rounded first, so the cast is exact"
)]
fn channel_byte(value: f32) -> u8 {
    value.clamp(0.0, 255.0).round() as u8
}

/// Every colour value in `text`: where it starts and its RGBA.
pub fn find(text: &str) -> Vec<(LineCol, [u8; 4])> {
    let mut out = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut idx = 0;
        while let Some(rest) = chars.get(idx..)
            && let Some((&chr, after)) = rest.split_first()
        {
            let word_start = idx
                .checked_sub(1)
                .and_then(|prev| chars.get(prev))
                .is_none_or(|prev| !is_name_char(*prev));
            let found = if chr == '#' {
                hex(after)
            } else if word_start && chr.is_ascii_alphabetic() {
                function(rest)
            } else {
                None
            };
            match found {
                Some((len, rgba)) => {
                    out.push((LineCol::new(to_u32(line_no), to_u32(idx)), rgba));
                    idx += len.max(1);
                }
                None => idx += 1,
            }
        }
    }
    out
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` (after the `#`): (chars used, colour).
fn hex(rest: &[char]) -> Option<(usize, [u8; 4])> {
    let count = rest
        .iter()
        .take_while(|chr| chr.is_ascii_hexdigit())
        .count();
    let (digits, after) = rest.split_at_checked(count)?;
    if after.first().is_some_and(|chr| is_name_char(*chr)) {
        return None;
    }
    let nibbles: Vec<u8> = digits
        .iter()
        .map(|chr| chr.to_digit(16).and_then(|value| u8::try_from(value).ok()))
        .collect::<Option<_>>()?;
    // One digit stands for itself twice (`f` is `ff`), two make a byte.
    let short = |nibble: u8| nibble * 17;
    let byte = |high: u8, low: u8| high * 16 + low;
    let rgba = match *nibbles.as_slice() {
        [red, green, blue] => [short(red), short(green), short(blue), 255],
        [red, green, blue, alpha] => [short(red), short(green), short(blue), short(alpha)],
        [red_hi, red_lo, green_hi, green_lo, blue_hi, blue_lo] => [
            byte(red_hi, red_lo),
            byte(green_hi, green_lo),
            byte(blue_hi, blue_lo),
            255,
        ],
        [
            red_hi,
            red_lo,
            green_hi,
            green_lo,
            blue_hi,
            blue_lo,
            alpha_hi,
            alpha_lo,
        ] => [
            byte(red_hi, red_lo),
            byte(green_hi, green_lo),
            byte(blue_hi, blue_lo),
            byte(alpha_hi, alpha_lo),
        ],
        _ => return None,
    };
    Some((count + 1, rgba))
}

/// `rgb(...)`, `rgba(...)`, `hsl(...)`, `hsla(...)` with comma or space syntax.
fn function(rest: &[char]) -> Option<(usize, [u8; 4])> {
    let name: String = rest
        .iter()
        .take_while(|chr| chr.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    if !matches!(name.as_str(), "rgb" | "rgba" | "hsl" | "hsla")
        || rest.get(name.len()) != Some(&'(')
    {
        return None;
    }
    let close = rest.iter().position(|chr| *chr == ')')?;
    let inner: String = rest.get(name.len() + 1..close)?.iter().collect();
    let parts: Vec<&str> = inner
        .split([',', ' ', '/'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let [first, second, third, tail @ ..] = parts.as_slice() else {
        return None;
    };
    // A component: a number, or a percentage of `full`.
    let num = |part: &str, full: f32| -> Option<f32> {
        match part.strip_suffix('%') {
            Some(percent) => Some(percent.parse::<f32>().ok()? / 100.0 * full),
            None => part.trim_end_matches("deg").parse::<f32>().ok(),
        }
    };
    let alpha = match tail.first() {
        Some(part) => channel_byte(num(part, 1.0)?.clamp(0.0, 1.0) * 255.0),
        None => 255,
    };
    let [red, green, blue] = if name.starts_with("rgb") {
        let channel = |part: &str| Some(channel_byte(num(part, 255.0)?));
        [channel(first)?, channel(second)?, channel(third)?]
    } else {
        hsl_to_rgb(num(first, 360.0)?, num(second, 1.0)?, num(third, 1.0)?)
    };
    Some((close + 1, [red, green, blue, alpha]))
}

fn hsl_to_rgb(hue: f32, sat: f32, light: f32) -> [u8; 3] {
    let hue = hue.rem_euclid(360.0) / 360.0;
    let (sat, light) = (sat.clamp(0.0, 1.0), light.clamp(0.0, 1.0));
    let upper = if light < 0.5 {
        light * (1.0 + sat)
    } else {
        light + sat - light * sat
    };
    let lower = 2.0 * light - upper;
    let channel = |phase: f32| {
        let phase = phase.rem_euclid(1.0);
        let value = if phase < 1.0 / 6.0 {
            lower + (upper - lower) * 6.0 * phase
        } else if phase < 0.5 {
            upper
        } else if phase < 2.0 / 3.0 {
            lower + (upper - lower) * (2.0 / 3.0 - phase) * 6.0
        } else {
            lower
        };
        channel_byte(value * 255.0)
    };
    [
        channel(hue + 1.0 / 3.0),
        channel(hue),
        channel(hue - 1.0 / 3.0),
    ]
}

/// `$name` (SCSS) and `@name` (Less) variables: (start, length in chars).
pub fn style_variables(text: &str, path: &Path) -> Vec<(LineCol, u32)> {
    let sigil = match path.extension().and_then(|ext| ext.to_str()) {
        Some("scss" | "sass") => '$',
        Some("less") => '@',
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut idx = 0;
        while let Some((&chr, after)) = chars.get(idx..).and_then(<[char]>::split_first) {
            if chr == sigil {
                let len = after.iter().take_while(|next| is_name_char(**next)).count();
                if len > 0 {
                    out.push((
                        LineCol::new(to_u32(line_no), to_u32(idx)),
                        to_u32(len).saturating_add(1),
                    ));
                }
                idx += len + 1;
            } else {
                idx += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_hex_and_functions() -> Result<(), Box<dyn std::error::Error>> {
        let found =
            find("a { color: #f00; background: rgba(10, 20, 30, 0.5); x: hsl(120 100% 50%); }");
        let colours: Vec<[u8; 4]> = found.iter().map(|entry| entry.1).collect();
        assert_eq!(
            colours,
            [[255, 0, 0, 255], [10, 20, 30, 128], [0, 255, 0, 255]]
        );
        let first = found.first().ok_or("no colour found")?;
        assert_eq!(first.0, LineCol::new(0, 11));
        Ok(())
    }

    #[test]
    fn ignores_ids_and_words() {
        assert_eq!(find("#header { } .rgbx { } #abcdefg"), []);
    }

    #[test]
    fn finds_scss_variables() {
        let vars = style_variables(
            "$primary: #fff;\na { color: $primary-dark; }",
            Path::new("x.scss"),
        );
        assert_eq!(vars, [(LineCol::new(0, 0), 8), (LineCol::new(1, 11), 13)]);
    }
}
