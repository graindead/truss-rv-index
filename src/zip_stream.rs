//! rv-index: reading a chart's note files from a zip whose server ignores HTTP ranges
//! (RhythmVerse's own download_file links). The zip is read from the start, one local file
//! header at a time: note files are unpacked, everything else is skipped without being kept, and
//! the download stops as soon as the note files are in (a chart file and song.ini, and the next
//! entry isn't another chart file). Zips usually list notes.* and song.ini before song.ogg and
//! video.*, so the big files are mostly never downloaded.
//!
//! When the download isn't a zip (a .7z / .rar / .sng) it is written to disk whole, to be
//! unpacked as before, so nothing is downloaded twice.

use crate::api::{CLIENT, count};
use futures::StreamExt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub enum Streamed {
    /// The note files were unpacked into the destination.
    Unpacked,
    /// Not a zip: the whole download, saved to this file.
    File(PathBuf),
    /// A zip layout this reader can't follow (stored entry with a data descriptor): download
    /// the whole file instead.
    Unsupported,
}

/// Byte source over a streamed response with a small look-ahead buffer.
struct Src {
    stream: std::pin::Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl Src {
    async fn fill(&mut self, n: usize) -> Result<bool, String> {
        while self.buf.len() - self.pos < n {
            if self.eof {
                return Ok(false);
            }
            match self.stream.next().await {
                Some(chunk) => {
                    let chunk = chunk.map_err(|e| format!("Connection lost: {e}"))?;
                    count(chunk.len());
                    if self.pos > 0 && self.pos == self.buf.len() {
                        self.buf.clear();
                        self.pos = 0;
                    } else if self.pos > (1 << 20) {
                        self.buf.drain(..self.pos);
                        self.pos = 0;
                    }
                    self.buf.extend_from_slice(&chunk);
                }
                None => self.eof = true,
            }
        }
        Ok(true)
    }
    fn peek(&self) -> &[u8] {
        &self.buf[self.pos..]
    }
    async fn take(&mut self, n: usize) -> Result<Vec<u8>, String> {
        if !self.fill(n).await? {
            return Err("The download was cut off by the server. Try again.".into());
        }
        let v = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(v)
    }
    /// Skips `n` bytes without keeping them.
    async fn skip(&mut self, mut n: u64) -> Result<(), String> {
        while n > 0 {
            if self.buf.len() == self.pos && !self.fill(1).await? {
                return Err("The download was cut off by the server. Try again.".into());
            }
            let have = ((self.buf.len() - self.pos) as u64).min(n);
            self.pos += have as usize;
            n -= have;
        }
        Ok(())
    }
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Zip64 sizes from a local header's extra field: (uncompressed, compressed).
fn zip64_sizes(extra: &[u8]) -> Option<(u64, u64)> {
    let mut p = 0;
    while p + 4 <= extra.len() {
        let (id, len) = (u16_at(extra, p), u16_at(extra, p + 2) as usize);
        if id == 1 && len >= 16 && p + 4 + 16 <= extra.len() {
            let a = u64::from_le_bytes(extra[p + 4..p + 12].try_into().ok()?);
            let b = u64::from_le_bytes(extra[p + 12..p + 20].try_into().ok()?);
            return Some((a, b));
        }
        p += 4 + len;
    }
    None
}

fn is_chart_file(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".chart") || l.ends_with(".mid")
}

/// Streams `url`, unpacking the entries `keep` accepts into `dest`.
pub async fn fetch(url: &str, work: &Path, dest: &Path, keep: impl Fn(&str) -> Option<PathBuf>) -> Result<Streamed, String> {
    let resp = CLIENT
        .get(url)
        .timeout(std::time::Duration::from_secs(60 * 30))
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            404 | 410 => "This chart is no longer available: its download link is dead (the file was deleted or moved).".into(),
            401 | 403 => "The host refused the download (the file may be private).".into(),
            429 => "The host is limiting downloads right now. Wait a bit and try again.".into(),
            c => format!("Download failed: the host returned {c}"),
        });
    }
    if resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/html")) {
        return Err("The host sent a web page instead of the file (it may be over its download limit or need a sign-in).".into());
    }
    let mut src = Src { stream: Box::pin(resp.bytes_stream()), buf: Vec::new(), pos: 0, eof: false };
    src.fill(4).await?;
    if !src.peek().starts_with(b"PK\x03\x04") {
        // Not a zip: keep the whole thing for unpacking.
        let path = work.join("download.bin");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
        out.write_all(src.peek()).map_err(|e| e.to_string())?;
        src.pos = src.buf.len();
        while let Some(chunk) = src.stream.next().await {
            let chunk = chunk.map_err(|e| format!("Connection lost: {e}"))?;
            count(chunk.len());
            out.write_all(&chunk).map_err(|e| e.to_string())?;
        }
        out.flush().map_err(|e| e.to_string())?;
        return Ok(Streamed::File(path));
    }

    let (mut got_chart, mut got_ini) = (false, false);
    loop {
        if !src.fill(30).await? || !src.peek().starts_with(b"PK\x03\x04") {
            // The central directory (or the end): every entry has been seen.
            return Ok(Streamed::Unpacked);
        }
        let h = src.take(30).await?;
        let flags = u16_at(&h, 6);
        let method = u16_at(&h, 8);
        let crc = u32_at(&h, 14);
        let (mut comp, mut size) = (u32_at(&h, 18) as u64, u32_at(&h, 22) as u64);
        let (n, x) = (u16_at(&h, 26) as usize, u16_at(&h, 28) as usize);
        let name_bytes = src.take(n).await?;
        let extra = src.take(x).await?;
        let name: String = if flags & 0x800 != 0 {
            String::from_utf8_lossy(&name_bytes).into_owned()
        } else {
            name_bytes.iter().map(|&b| b as char).collect()
        };
        if (comp == u32::MAX as u64 || size == u32::MAX as u64)
            && let Some((s, c)) = zip64_sizes(&extra)
        {
            (size, comp) = (s, c);
        }
        let is_dir = name.ends_with('/') || name.ends_with('\\');
        // Done once the note files are in and the next entry isn't another chart file.
        if got_chart && got_ini && !is_dir && !is_chart_file(&name) {
            return Ok(Streamed::Unpacked);
        }
        let descriptor = flags & 8 != 0;
        let wanted = if is_dir || flags & 1 != 0 { None } else { keep(&name) };
        if descriptor && method != 8 {
            return Ok(Streamed::Unsupported);
        }
        let data: Option<Vec<u8>> = if descriptor {
            // Sizes come after the data: inflate to find where the entry ends.
            let mut z = flate2::Decompress::new(false);
            let mut out = Vec::new();
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                if src.buf.len() == src.pos && !src.fill(1).await? {
                    return Err("The download was cut off by the server. Try again.".into());
                }
                let before_in = z.total_in();
                let before_out = z.total_out();
                let st = z
                    .decompress(&src.buf[src.pos..], &mut chunk, flate2::FlushDecompress::None)
                    .map_err(|e| format!("Couldn't unpack: {e}"))?;
                src.pos += (z.total_in() - before_in) as usize;
                if wanted.is_some() {
                    out.extend_from_slice(&chunk[..(z.total_out() - before_out) as usize]);
                }
                if st == flate2::Status::StreamEnd {
                    break;
                }
            }
            // The data descriptor: optional signature, crc, then 32- or 64-bit sizes.
            src.fill(4).await?;
            if src.peek().starts_with(b"PK\x07\x08") {
                src.skip(4).await?;
            }
            let d = src.take(12).await?;
            let crc = u32_at(&d, 0);
            // A zip64 descriptor has 8-byte sizes; tell by what follows.
            if src.fill(4).await? && !src.peek().starts_with(b"PK") {
                src.skip(8).await?;
            }
            match wanted {
                Some(_) if crc32fast::hash(&out) != crc => None,
                _ => Some(out),
            }
        } else if wanted.is_some() {
            let raw = src.take(comp as usize).await?;
            let bytes = if method == 0 {
                raw
            } else if method == 8 {
                let mut v = Vec::with_capacity(size as usize);
                flate2::read::DeflateDecoder::new(&raw[..]).read_to_end(&mut v).map_err(|e| format!("Couldn't unpack: {e}"))?;
                v
            } else {
                return Ok(Streamed::Unsupported);
            };
            if bytes.len() as u64 != size || crc32fast::hash(&bytes) != crc {
                None
            } else {
                Some(bytes)
            }
        } else {
            src.skip(comp).await?;
            Some(Vec::new())
        };
        if let Some(rel) = wanted {
            let bytes = data.ok_or_else(|| format!("\"{name}\" didn't download correctly (checksum mismatch). Try again."))?;
            let out = dest.join(&rel);
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            std::fs::write(&out, bytes).map_err(|e| format!("Couldn't save: {e}"))?;
            let file = rel.file_name().map(|f| f.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if is_chart_file(&file) {
                got_chart = true;
            } else if file.ends_with(".ini") {
                got_ini = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local HTTP server that ignores ranges, serving `body` once.
    async fn serve(body: Vec<u8>) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let s2 = sent.clone();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 4096];
            let _ = sock.read(&mut req).await;
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = sock.write_all(head.as_bytes()).await;
            for c in body.chunks(8192) {
                if sock.write_all(c).await.is_err() {
                    break;
                }
                s2.fetch_add(c.len(), std::sync::atomic::Ordering::Relaxed);
            }
        });
        (format!("http://{addr}/chart.zip"), sent)
    }

    fn make_zip() -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let deflate = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            z.start_file("Song/guitar.ogg", stored).unwrap();
            z.write_all(&vec![1u8; 20_000]).unwrap();
            z.start_file("Song/notes.chart", deflate).unwrap();
            z.write_all(&b"[Song]\n".repeat(300)).unwrap();
            z.start_file("Song/song.ini", deflate).unwrap();
            z.write_all(b"[song]\nname = x\n").unwrap();
            z.start_file("Song/video.mp4", stored).unwrap();
            z.write_all(&vec![7u8; 8_000_000]).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[tokio::test]
    async fn stops_before_the_video() {
        let bytes = make_zip();
        let total = bytes.len();
        let (url, sent) = serve(bytes).await;
        let dir = std::env::temp_dir().join(format!("rv-zipstream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keep = |n: &str| {
            let l = n.to_ascii_lowercase();
            (l.ends_with(".chart") || l.ends_with(".ini") || l.ends_with(".mid")).then(|| PathBuf::from(n))
        };
        let r = fetch(&url, &dir, &dir.join("chart"), keep).await.unwrap();
        assert!(matches!(r, Streamed::Unpacked));
        assert_eq!(std::fs::read(dir.join("chart/Song/notes.chart")).unwrap(), b"[Song]\n".repeat(300));
        assert!(dir.join("chart/Song/song.ini").exists());
        assert!(!dir.join("chart/Song/video.mp4").exists());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let sent = sent.load(std::sync::atomic::Ordering::Relaxed);
        assert!(sent < total / 2, "sent {sent} of {total}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
