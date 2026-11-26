//! Where sanitty keeps state outside a project (review comments, saved
//! queries), and the hash that names it.
//!
//! The names hold a hash of the workspace path. It must give the same value
//! in every build: Rust's `DefaultHasher` does not promise that across
//! compiler releases, so a toolchain upgrade could orphan saved state. FNV-1a
//! over the path's bytes never changes.

use std::path::Path;
use std::path::PathBuf;

/// A 64-bit FNV-1a hash, the same in every build.
#[derive(Clone, Copy, Debug)]
pub struct StableHash(u64);

impl StableHash {
    pub fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    /// Add bytes, then a separator, so `("ab", "c")` and `("a", "bc")`
    /// differ.
    pub fn add(mut self, bytes: &[u8]) -> Self {
        for byte in bytes.iter().chain(std::iter::once(&0xff)) {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
        self
    }

    pub fn finish(self) -> u64 {
        self.0
    }
}

/// `<state dir>/sanitty/<kind>-<workspace name>-<hash><suffix>`, for one
/// workspace's state of one kind (`review` with `.json`, `db` with no
/// suffix for a directory). State kept under the name earlier builds used
/// is moved to this name the first time it is asked for.
pub fn path(kind: &str, root: &Path, suffix: &str) -> Option<PathBuf> {
    let dir = dirs::state_dir()
        .or_else(dirs::data_local_dir)?
        .join("sanitty");
    let name = root.file_name().map_or_else(
        || "root".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let stable = StableHash::new()
        .add(root.as_os_str().as_encoded_bytes())
        .finish();
    let current = dir.join(format!("{kind}-{name}-{stable:016x}{suffix}"));
    if !current.exists() {
        let earlier = dir.join(format!("{kind}-{name}-{:016x}{suffix}", earlier_hash(root)));
        if earlier.exists() {
            let _ = std::fs::rename(&earlier, &current);
        }
    }
    Some(current)
}

/// The hash earlier builds named state with (`DefaultHasher`), to find state
/// saved before the names became stable.
fn earlier_hash(root: &Path) -> u64 {
    use std::hash::Hash as _;
    use std::hash::Hasher as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    hasher.finish()
}

/// Write `text` to `path` in one step: to a file next to it first, then
/// renamed over it, so a crash never leaves half a file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_never_changes() {
        // Fixed values: a change here would orphan everyone's saved state.
        assert_eq!(StableHash::new().finish(), 0xcbf2_9ce4_8422_2325);
        assert_eq!(
            StableHash::new().add(b"/home/robin/project").finish(),
            0x8936_9b52_e7e0_73bd
        );
        assert_ne!(
            StableHash::new().add(b"ab").add(b"c").finish(),
            StableHash::new().add(b"a").add(b"bc").finish()
        );
    }
}
