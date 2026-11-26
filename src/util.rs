//! Small helpers used across the code base.

use std::path::Path;

/// A count as `u32`, saturating: no text gets near the limit.
pub fn to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// A count as `u16` (a screen size), saturating.
pub fn to_u16(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// Whether a program of this name is on `PATH`.
pub fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file()))
}

/// Files with colour values: style sheets and the markup that holds them.
pub fn is_style_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("css" | "scss" | "sass" | "less" | "html" | "htm" | "vue" | "svelte")
    )
}

/// `word` or `words`, for `count` of them.
pub fn plural_word(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

/// "1 comment", "3 comments".
pub fn counted(count: usize, word: &str) -> String {
    format!("{count} {}", plural_word(count, word))
}
