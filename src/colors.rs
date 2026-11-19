//! Colour values in style files (`#3366ff`, `rgb(...)`, `hsl(...)`), so the
//! editor can show a swatch in front of each, and SCSS/Less variables, which
//! the CSS grammar (used for those files) leaves uncoloured.

use std::path::Path;

use karet_core::LineCol;

/// Files whose colour values get swatches.
pub fn wants_swatches(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("css" | "scss" | "sass" | "less" | "html" | "htm" | "vue" | "svelte")
    )
}

/// Every colour value in `text`: where it starts and its RGBA.
pub fn find(text: &str) -> Vec<(LineCol, [u8; 4])> {
    let mut out = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let word_start = i == 0
                || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '-' || chars[i - 1] == '_');
            let found = if chars[i] == '#' {
                hex(&chars[i + 1..])
            } else if word_start && chars[i].is_ascii_alphabetic() {
                function(&chars[i..])
            } else {
                None
            };
            match found {
                Some((len, rgba)) => {
                    out.push((LineCol::new(line_no as u32, i as u32), rgba));
                    i += len.max(1);
                }
                None => i += 1,
            }
        }
    }
    out
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` (after the `#`): (chars used, colour).
fn hex(rest: &[char]) -> Option<(usize, [u8; 4])> {
    let n = rest.iter().take_while(|c| c.is_ascii_hexdigit()).count();
    let boundary = rest
        .get(n)
        .is_none_or(|c| !(c.is_alphanumeric() || *c == '_' || *c == '-'));
    if !boundary {
        return None;
    }
    let digits: String = rest[..n].iter().collect();
    let d = |s: &str| u8::from_str_radix(s, 16).ok();
    let short = |c: char| d(&format!("{c}{c}"));
    let ch: Vec<char> = digits.chars().collect();
    let rgba = match n {
        3 => [short(ch[0])?, short(ch[1])?, short(ch[2])?, 255],
        4 => [short(ch[0])?, short(ch[1])?, short(ch[2])?, short(ch[3])?],
        6 => [d(&digits[0..2])?, d(&digits[2..4])?, d(&digits[4..6])?, 255],
        8 => [
            d(&digits[0..2])?,
            d(&digits[2..4])?,
            d(&digits[4..6])?,
            d(&digits[6..8])?,
        ],
        _ => return None,
    };
    Some((n + 1, rgba))
}

/// `rgb(...)`, `rgba(...)`, `hsl(...)`, `hsla(...)` with comma or space syntax.
fn function(rest: &[char]) -> Option<(usize, [u8; 4])> {
    let name: String = rest
        .iter()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    if !matches!(name.as_str(), "rgb" | "rgba" | "hsl" | "hsla")
        || rest.get(name.len()) != Some(&'(')
    {
        return None;
    }
    let close = rest.iter().position(|c| *c == ')')?;
    let inner: String = rest[name.len() + 1..close].iter().collect();
    let parts: Vec<&str> = inner
        .split([',', ' ', '/'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    // A component: a number, or a percentage of `full`.
    let num = |p: &str, full: f32| -> Option<f32> {
        match p.strip_suffix('%') {
            Some(v) => Some(v.parse::<f32>().ok()? / 100.0 * full),
            None => p.trim_end_matches("deg").parse::<f32>().ok(),
        }
    };
    let alpha = match parts.get(3) {
        Some(a) => (num(a, 1.0)?.clamp(0.0, 1.0) * 255.0).round() as u8,
        None => 255,
    };
    let rgb = if name.starts_with("rgb") {
        let c = |p: &str| Some(num(p, 255.0)?.clamp(0.0, 255.0).round() as u8);
        [c(parts[0])?, c(parts[1])?, c(parts[2])?]
    } else {
        hsl_to_rgb(
            num(parts[0], 360.0)?,
            num(parts[1], 1.0)?,
            num(parts[2], 1.0)?,
        )
    };
    Some((close + 1, [rgb[0], rgb[1], rgb[2], alpha]))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 360.0;
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

/// `$name` (SCSS) and `@name` (Less) variables: (start, length in chars).
pub fn style_variables(text: &str, path: &Path) -> Vec<(LineCol, u32)> {
    let sigil = match path.extension().and_then(|e| e.to_str()) {
        Some("scss" | "sass") => '$',
        Some("less") => '@',
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == sigil {
                let len = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '-' || **c == '_')
                    .count();
                if len > 0 {
                    out.push((LineCol::new(line_no as u32, i as u32), len as u32 + 1));
                }
                i += len + 1;
            } else {
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_hex_and_functions() {
        let found =
            find("a { color: #f00; background: rgba(10, 20, 30, 0.5); x: hsl(120 100% 50%); }");
        let colours: Vec<[u8; 4]> = found.iter().map(|f| f.1).collect();
        assert_eq!(
            colours,
            [[255, 0, 0, 255], [10, 20, 30, 128], [0, 255, 0, 255]]
        );
        assert_eq!(found[0].0, LineCol::new(0, 11));
    }

    #[test]
    fn ignores_ids_and_words() {
        assert!(find("#header { } .rgbx { } #abcdefg").is_empty());
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
