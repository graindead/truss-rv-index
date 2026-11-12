//! The parts of Truss's download.rs that the copied RhythmVerse code uses (unchanged).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum Progress {
    Status(String),
    Bytes { done: u64, total: Option<u64> },
    Finished(Result<PathBuf, String>),
}

/// Deletes the temporary download location if the download is cancelled
/// (task dropped) or fails.
pub struct TempGuard(pub Option<PathBuf>);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}


/// Mirrors the `sanitize-filename` npm package with Bridge's look-alike replacements.
pub fn sanitize_filename(input: &str) -> String {
    let mut s: String = input
        .chars()
        .map(|c| match c {
            '<' => '❮',
            '>' => '❯',
            ':' => '꞉',
            '"' => '\'',
            '/' => '／',
            '\\' => '⧵',
            '|' => '⏐',
            '?' => '？',
            '*' => '⁎',
            c if (c as u32) < 0x20 || (0x80..=0x9f).contains(&(c as u32)) => '_',
            c => c,
        })
        .collect();

    if !s.is_empty() && s.chars().all(|c| c == '.') {
        s = "_".repeat(s.len());
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    // Windows strips trailing dots/spaces, which would break paths.
    let trimmed_len = s.trim_end_matches(['.', ' ']).len();
    if trimmed_len < s.len() {
        s.truncate(trimmed_len);
    }
    // Limit to 255 bytes on a char boundary.
    if s.len() > 255 {
        let mut end = 255;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}
