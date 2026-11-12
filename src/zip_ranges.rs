//! Downloads only some files from a remote .zip, using HTTP range requests: the zip's table of
//! contents (central directory) is at the end of the file and records where each file's bytes
//! are, so the chart, audio and art can be fetched without the (often huge) video.
//!
//! Returns `Ok(false)` when this can't be done (server without range support, not a zip, zip64,
//! encryption…), so the caller can fall back to downloading the whole file.

use crate::api::CLIENT;
use futures::{SinkExt, StreamExt, channel::mpsc::Sender};
use std::io::Read;
use std::path::{Path, PathBuf};

const TAIL: u64 = 128 * 1024;

struct Entry {
    rel: PathBuf,
    method: u16,
    crc: u32,
    comp_size: u64,
    size: u64,
    local_offset: u64,
}

/// Bytes `start..=end` of `url`; `Ok(None)` if the server ignores ranges. Also returns the
/// file's total size from Content-Range.
async fn range(url: &str, spec: &str) -> Result<Option<(Vec<u8>, Option<u64>)>, String> {
    let resp = CLIENT
        .get(url)
        .header("Range", format!("bytes={spec}"))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if resp.status().as_u16() != 206 {
        return Ok(None); // dropping the response cancels the transfer
    }
    let total = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse().ok());
    let bytes = resp.bytes().await.map_err(|e| format!("Connection lost: {e}"))?;
    crate::api::count(bytes.len()); // rv-index: bytes fetched
    Ok(Some((bytes.to_vec(), total)))
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Parses the central directory. `None` = unsupported layout (fall back).
fn parse_directory(cd: &[u8], keep: &dyn Fn(&str) -> Option<PathBuf>) -> Option<(Vec<Entry>, Vec<u64>)> {
    let mut entries = Vec::new();
    let mut all_offsets = Vec::new();
    let mut p = 0;
    while p + 46 <= cd.len() && u32_at(cd, p)? == 0x0201_4b50 {
        let flags = u16_at(cd, p + 8)?;
        let method = u16_at(cd, p + 10)?;
        let crc = u32_at(cd, p + 16)?;
        let comp = u32_at(cd, p + 20)?;
        let size = u32_at(cd, p + 24)?;
        let (n, e, c) = (u16_at(cd, p + 28)? as usize, u16_at(cd, p + 30)? as usize, u16_at(cd, p + 32)? as usize);
        let local = u32_at(cd, p + 42)?;
        if comp == u32::MAX || size == u32::MAX || local == u32::MAX {
            return None; // zip64
        }
        let name_bytes = cd.get(p + 46..p + 46 + n)?;
        // Bit 11: UTF-8 names; otherwise old DOS code page (treat as Latin-1).
        let name: String = if flags & 0x800 != 0 {
            String::from_utf8_lossy(name_bytes).into_owned()
        } else {
            name_bytes.iter().map(|&b| b as char).collect()
        };
        all_offsets.push(local as u64);
        let is_dir = name.ends_with('/') || name.ends_with('\\');
        if !is_dir && let Some(rel) = keep(&name) {
            if flags & 1 != 0 || !(method == 0 || method == 8) {
                return None; // encrypted or an unusual compression method
            }
            entries.push(Entry { rel, method, crc, comp_size: comp as u64, size: size as u64, local_offset: local as u64 });
        }
        p += 46 + n + e + c;
    }
    Some((entries, all_offsets))
}

/// Fetches the kept files of the zip at `url` into `dest`. `keep` maps an entry name to its
/// destination (or `None` to skip it).
pub async fn fetch_selected(
    url: &str,
    dest: &Path,
    keep: impl Fn(&str) -> Option<PathBuf>,
    tx: &mut Sender<crate::download::Progress>,
) -> Result<bool, String> {
    use crate::download::Progress;
    // 1. The file size (a 1-byte range reports it; some servers ignore "last N bytes" ranges),
    //    then the end of the file: the end-of-directory record and usually the whole directory.
    let Some((_, Some(total))) = range(url, "0-0").await? else { return Ok(false) };
    if total < 22 {
        return Ok(false);
    }
    let tail_from = total.saturating_sub(TAIL);
    let Some((tail, _)) = range(url, &format!("{tail_from}-{}", total - 1)).await? else { return Ok(false) };
    if tail.len() as u64 != total - tail_from {
        return Ok(false);
    }
    let Some(eocd) = tail.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]) else { return Ok(false) };
    let (Some(cd_size), Some(cd_offset)) = (u32_at(&tail, eocd + 12), u32_at(&tail, eocd + 16)) else { return Ok(false) };
    if cd_size == u32::MAX || cd_offset == u32::MAX {
        return Ok(false);
    }
    let (cd_size, cd_offset) = (cd_size as u64, cd_offset as u64);
    let tail_start = total - tail.len() as u64;
    let cd: Vec<u8> = if cd_offset >= tail_start {
        let from = (cd_offset - tail_start) as usize;
        tail.get(from..from + cd_size as usize).map(<[u8]>::to_vec).unwrap_or_default()
    } else {
        match range(url, &format!("{cd_offset}-{}", cd_offset + cd_size - 1)).await? {
            Some((b, _)) => b,
            None => return Ok(false),
        }
    };
    let Some((mut wanted, mut offsets)) = parse_directory(&cd, &keep) else { return Ok(false) };
    if wanted.is_empty() {
        return Ok(false);
    }
    // 2. Where each wanted file's bytes end: at the next file's header (or the directory).
    offsets.push(cd_offset);
    offsets.sort_unstable();
    offsets.dedup();
    wanted.sort_by_key(|e| e.local_offset);
    let span_end = |off: u64| offsets.iter().copied().find(|&o| o > off).unwrap_or(cd_offset);
    // Merge neighbouring wanted files into a few requests.
    let mut groups: Vec<(u64, u64, Vec<usize>)> = Vec::new();
    for (i, e) in wanted.iter().enumerate() {
        let end = span_end(e.local_offset);
        match groups.last_mut() {
            Some(g) if g.1 == e.local_offset => {
                g.1 = end;
                g.2.push(i);
            }
            _ => groups.push((e.local_offset, end, vec![i])),
        }
    }
    let to_fetch: u64 = groups.iter().map(|g| g.1 - g.0).sum();
    let _ = tx
        .send(Progress::Status(format!(
            "Skipping the video: downloading {:.1} of {:.1} MB",
            to_fetch as f64 / 1_048_576.0,
            total as f64 / 1_048_576.0
        )))
        .await;
    // 3. Download each group and unpack its files.
    let mut done = 0u64;
    for (start, end, idxs) in groups {
        let resp = CLIENT
            .get(url)
            .header("Range", format!("bytes={start}-{}", end - 1))
            .timeout(std::time::Duration::from_secs(60 * 30))
            .send()
            .await
            .map_err(|e| format!("Download failed: {e}"))?;
        if resp.status().as_u16() != 206 {
            return Ok(false);
        }
        let mut buf = Vec::with_capacity((end - start) as usize);
        let mut stream = resp.bytes_stream();
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("Connection lost: {e}"))?;
            crate::api::count(chunk.len()); // rv-index: bytes fetched
            buf.extend_from_slice(&chunk);
            if last.elapsed() >= std::time::Duration::from_millis(33) {
                last = std::time::Instant::now();
                let _ = tx.try_send(Progress::Bytes { done: done + buf.len() as u64, total: Some(to_fetch) });
            }
        }
        if (buf.len() as u64) < end - start {
            return Err("The download was cut off by the server. Try again.".into());
        }
        done += buf.len() as u64;
        let _ = tx.send(Progress::Bytes { done, total: Some(to_fetch) }).await;
        for i in idxs {
            let e = &wanted[i];
            let base = (e.local_offset - start) as usize;
            if u32_at(&buf, base) != Some(0x0403_4b50) {
                return Err("The zip file is damaged".into());
            }
            let n = u16_at(&buf, base + 26).ok_or("The zip file is damaged")? as usize;
            let x = u16_at(&buf, base + 28).ok_or("The zip file is damaged")? as usize;
            let data_start = base + 30 + n + x;
            let data = buf.get(data_start..data_start + e.comp_size as usize).ok_or("The zip file is damaged")?;
            let out = dest.join(&e.rel);
            write_file(&out, e, data)?;
        }
    }
    Ok(true)
}

fn write_file(out: &Path, e: &Entry, data: &[u8]) -> Result<(), String> {
    if let Some(p) = out.parent() {
        std::fs::create_dir_all(p).map_err(|err| err.to_string())?;
    }
    let bytes: Vec<u8> = if e.method == 0 {
        data.to_vec()
    } else {
        let mut v = Vec::with_capacity(e.size as usize);
        flate2::read::DeflateDecoder::new(data).read_to_end(&mut v).map_err(|err| format!("Couldn't unpack: {err}"))?;
        v
    };
    if bytes.len() as u64 != e.size || crc32fast::hash(&bytes) != e.crc {
        return Err(format!("\"{}\" didn't download correctly (checksum mismatch). Try again.", e.rel.display()));
    }
    std::fs::write(out, bytes).map_err(|err| format!("Couldn't save: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Network test: TRUSS_TEST_ZIP_URL + TRUSS_TEST_ZIP_DIR fetch a real zip without its video.
    #[test]
    fn live_partial() {
        let (Ok(url), Ok(dir)) = (std::env::var("TRUSS_TEST_ZIP_URL"), std::env::var("TRUSS_TEST_ZIP_DIR")) else { return };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (mut tx, mut rx) = futures::channel::mpsc::channel(256);
            let log = tokio::spawn(async move {
                let mut last = None;
                while let Some(p) = rx.next().await {
                    if let crate::download::Progress::Status(s) = &p {
                        println!("{s}");
                    }
                    last = Some(p);
                }
                last
            });
            let t = std::time::Instant::now();
            let keep = |n: &str| {
                let l = n.to_ascii_lowercase();
                (!l.contains("video.") && !l.ends_with('/')).then(|| PathBuf::from(n))
            };
            let r = fetch_selected(&url, Path::new(&dir), keep, &mut tx).await;
            drop(tx);
            println!("result {r:?} in {:?}; last progress {:?}", t.elapsed(), log.await.unwrap());
        });
    }

    /// Builds a zip in memory and checks the directory parsing and per-file unpacking.
    #[test]
    fn parses_and_unpacks() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let deflate = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            z.start_file("Song/notes.chart", deflate).unwrap();
            z.write_all(&b"[Song]\n".repeat(200)).unwrap();
            z.start_file("Song/video.mp4", stored).unwrap();
            z.write_all(&vec![7u8; 50_000]).unwrap();
            z.start_file("Song/song.ini", stored).unwrap();
            z.write_all(b"[song]\nname = x\n").unwrap();
            z.finish().unwrap();
        }
        let bytes = buf.into_inner();
        let eocd = bytes.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        let (size, off) = (u32_at(&bytes, eocd + 12).unwrap() as usize, u32_at(&bytes, eocd + 16).unwrap() as usize);
        let keep = |n: &str| (!n.contains("video")).then(|| PathBuf::from(n));
        let (wanted, offsets) = parse_directory(&bytes[off..off + size], &keep).unwrap();
        assert_eq!(wanted.len(), 2);
        assert_eq!(offsets.len(), 3);
        let dir = std::env::temp_dir().join(format!("truss-zipranges-{}", std::process::id()));
        for e in &wanted {
            let base = e.local_offset as usize;
            let n = u16_at(&bytes, base + 26).unwrap() as usize;
            let x = u16_at(&bytes, base + 28).unwrap() as usize;
            let data = &bytes[base + 30 + n + x..base + 30 + n + x + e.comp_size as usize];
            write_file(&dir.join(&e.rel), e, data).unwrap();
        }
        assert_eq!(std::fs::read(dir.join("Song/notes.chart")).unwrap(), b"[Song]\n".repeat(200));
        assert!(!dir.join("Song/video.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
