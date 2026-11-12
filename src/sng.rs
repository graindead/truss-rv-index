//! Streaming .sng (SNGPKG) extractor.
//!
//! Bytes are fed in as they arrive from the network and written straight to
//! disk, so a chart is extracted by the time the download finishes.
//! Format: https://github.com/mdsitton/SngFileFormat

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const HEADER_FIXED: usize = 6 + 4 + 16; // "SNGPKG" + version + xor mask

struct FileMeta {
    name: String,
    len: u64,
    index: u64,
}

enum State {
    Header(Vec<u8>),
    Files {
        files: Vec<FileMeta>,
        current: usize,
        writer: Option<BufWriter<File>>,
        written: u64,
    },
    Done,
}

pub struct SngExtractor {
    dir: PathBuf,
    mask: [u8; 16],
    /// Absolute byte offset in the .sng stream of the next byte to be fed.
    pos: u64,
    state: State,
}

impl SngExtractor {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf(), mask: [0; 16], pos: 0, state: State::Header(Vec::with_capacity(64 * 1024)) }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.state, State::Done)
    }

    pub fn feed(&mut self, mut chunk: &[u8]) -> Result<(), String> {
        while !chunk.is_empty() {
            match &mut self.state {
                State::Done => return Ok(()), // trailing bytes are ignored
                State::Header(buf) => {
                    buf.extend_from_slice(chunk);
                    self.pos += chunk.len() as u64;
                    chunk = &[];
                    if let Some((files, mask, data_start, metadata)) = try_parse_header(buf)? {
                        self.mask = mask;
                        std::fs::write(self.dir.join("song.ini"), generate_ini(&metadata))
                            .map_err(|e| format!("Failed to write song.ini: {e}"))?;
                        let leftover = buf[data_start..].to_vec();
                        self.pos = data_start as u64;
                        self.state = State::Files { files, current: 0, writer: None, written: 0 };
                        self.feed(&leftover)?;
                    }
                }
                State::Files { files, current, writer, written } => {
                    let Some(f) = files.get(*current) else {
                        self.state = State::Done;
                        continue;
                    };
                    let file_start = f.index + *written;
                    // Skip padding between files, if any.
                    if self.pos < file_start {
                        let skip = (file_start - self.pos).min(chunk.len() as u64) as usize;
                        chunk = &chunk[skip..];
                        self.pos += skip as u64;
                        continue;
                    }
                    if writer.is_none() {
                        let path = self.dir.join(&f.name);
                        let file = File::create(&path).map_err(|e| format!("Failed to create {}: {e}", f.name))?;
                        *writer = Some(BufWriter::with_capacity(1 << 20, file));
                    }
                    let take = (f.len - *written).min(chunk.len() as u64) as usize;
                    let mut out = chunk[..take].to_vec();
                    unmask(&mut out, &self.mask, *written);
                    writer.as_mut().unwrap().write_all(&out).map_err(|e| format!("Write failed: {e}"))?;
                    *written += take as u64;
                    self.pos += take as u64;
                    chunk = &chunk[take..];
                    if *written >= f.len {
                        if let Some(mut w) = writer.take() {
                            w.flush().map_err(|e| format!("Write failed: {e}"))?;
                        }
                        *current += 1;
                        *written = 0;
                        if *current >= files.len() {
                            self.state = State::Done;
                        }
                    }
                }
            }
        }
        // Zero-length files never receive bytes; create them when reached.
        self.drain_empty_files()
    }

    fn drain_empty_files(&mut self) -> Result<(), String> {
        if let State::Files { files, current, writer, .. } = &mut self.state {
            while writer.is_none() && files.get(*current).is_some_and(|f| f.len == 0) {
                File::create(self.dir.join(&files[*current].name)).map_err(|e| e.to_string())?;
                *current += 1;
            }
            if *current >= files.len() {
                self.state = State::Done;
            }
        }
        Ok(())
    }
}

/// Contents of a .sng file read fully into memory (used by the chart preview).
pub struct SngContents {
    pub metadata: Vec<(String, String)>,
    pub files: Vec<(String, Vec<u8>)>,
}

impl SngContents {
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
}

/// Parses a complete .sng buffer, keeping only files for which `keep(name)` is true.
pub fn read_sng(data: &[u8], keep: impl Fn(&str) -> bool) -> Result<SngContents, String> {
    let (files, mask, _, metadata) =
        try_parse_header(data)?.ok_or("Incomplete .sng file")?;
    let mut out = Vec::new();
    for f in files {
        if !keep(&f.name) {
            continue;
        }
        let start = f.index as usize;
        let end = start.checked_add(f.len as usize).filter(|e| *e <= data.len()).ok_or("Truncated .sng file")?;
        let mut bytes = data[start..end].to_vec();
        unmask(&mut bytes, &mask, 0);
        out.push((f.name, bytes));
    }
    Ok(SngContents { metadata, files: out })
}

/// Byte range `start..end` of a remote file. A server that ignores ranges sends the whole file,
/// which is sliced here.
async fn get_range(url: &str, start: u64, end: u64) -> Result<Vec<u8>, String> {
    let resp = crate::api::CLIENT
        .get(url)
        .header("Range", format!("bytes={start}-{}", end.saturating_sub(1)))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't read the chart: {e}"))?;
    let whole = resp.status() == reqwest::StatusCode::OK;
    let bytes = resp.bytes().await.map_err(|e| format!("Couldn't read the chart: {e}"))?;
    crate::api::count(bytes.len()); // rv-index: bytes fetched
    Ok(if whole { bytes.get(start as usize..(end as usize).min(bytes.len())).unwrap_or_default().to_vec() } else { bytes.to_vec() })
}

/// The metadata and chart files (notes.chart / notes.mid) of a remote .sng, fetched by byte
/// range: only the header and the chart itself come down, not the audio.
pub async fn fetch_remote_notes(url: &str) -> Result<(Vec<(String, String)>, Vec<(String, Vec<u8>)>), String> {
    let mut want = 64 * 1024;
    let (files, mask, metadata) = loop {
        let head = get_range(url, 0, want).await?;
        if let Some((files, mask, _, metadata)) = try_parse_header(&head)? {
            break (files, mask, metadata);
        }
        if (head.len() as u64) < want || want >= 16 << 20 {
            return Err("Incomplete .sng file".into());
        }
        want *= 4;
    };
    let mut out = Vec::new();
    for f in files {
        let lower = f.name.to_ascii_lowercase();
        if !(lower.ends_with(".chart") || lower.ends_with(".mid")) {
            continue;
        }
        let mut bytes = get_range(url, f.index, f.index + f.len).await?;
        if bytes.len() as u64 != f.len {
            return Err("Truncated .sng file".into());
        }
        unmask(&mut bytes, &mask, 0);
        out.push((f.name, bytes));
    }
    Ok((metadata, out))
}

// ---------- Reading a .sng file on disk without extracting it ----------

/// A file that is deleted when the last handle to this guard is dropped.
pub struct TempFile(pub PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Opens a file so that it can still be deleted while open (Windows otherwise refuses).
pub fn open_shared(path: &Path, write: bool) -> std::io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    if write {
        o.write(true).create(true).truncate(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0x1 | 0x2 | 0x4); // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    }
    o.open(path)
}

pub struct SngEntry {
    pub name: String,
    pub index: u64,
    pub len: u64,
}

pub struct SngIndex {
    pub metadata: Vec<(String, String)>,
    pub files: Vec<SngEntry>,
    pub mask: [u8; 16],
}

impl SngIndex {
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
}

/// Reads just the header (metadata + file table) of a .sng file.
pub fn read_index(path: &Path) -> Result<SngIndex, String> {
    use std::io::Read;
    let mut f = open_shared(path, false).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Incomplete .sng file".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some((files, mask, _, metadata)) = try_parse_header(&buf)? {
            let files = files.into_iter().map(|f| SngEntry { name: f.name, index: f.index, len: f.len }).collect();
            return Ok(SngIndex { metadata, files, mask });
        }
    }
}

/// Reads and unmasks one (small) entry fully into memory, e.g. the chart file.
pub fn read_entry(path: &Path, idx: &SngIndex, e: &SngEntry) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut s = MaskedSlice::open(path, idx, e, None).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(e.len as usize);
    s.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A seekable reader over one entry inside a .sng file on disk, unmasking bytes as they're read.
/// Audio streams from here, so even a two-hour chart uses almost no memory.
pub struct MaskedSlice {
    file: File,
    start: u64,
    len: u64,
    pos: u64,
    mask: [u8; 16],
    _keep: Option<std::sync::Arc<TempFile>>,
}

impl MaskedSlice {
    pub fn open(
        path: &Path,
        idx: &SngIndex,
        e: &SngEntry,
        keep: Option<std::sync::Arc<TempFile>>,
    ) -> std::io::Result<Self> {
        Ok(Self { file: open_shared(path, false)?, start: e.index, len: e.len, pos: 0, mask: idx.mask, _keep: keep })
    }

    pub fn len(&self) -> u64 {
        self.len
    }
}

impl std::io::Read for MaskedSlice {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::{Seek, SeekFrom};
        let remaining = self.len.saturating_sub(self.pos);
        let want = (buf.len() as u64).min(remaining) as usize;
        if want == 0 {
            return Ok(0);
        }
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.file.read(&mut buf[..want])?;
        unmask(&mut buf[..n], &self.mask, self.pos);
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for MaskedSlice {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        let new = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(d) => self.len as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if new < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

/// XOR-unmasks `data`, which starts at byte `offset` within its file.
#[inline]
fn unmask(data: &mut [u8], mask: &[u8; 16], offset: u64) {
    // The key repeats every 256 bytes: key[i] = mask[i % 16] ^ (i % 256).
    let mut table = [0u8; 256];
    for (i, k) in table.iter_mut().enumerate() {
        *k = mask[i % 16] ^ i as u8;
    }
    let start = (offset % 256) as usize;
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= table[(start + i) & 0xFF];
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn i32(&mut self) -> Option<i32> {
        self.take(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()))
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn string(&mut self, n: usize) -> Option<String> {
        self.take(n).map(|b| String::from_utf8_lossy(b).into_owned())
    }
}

type Header = (Vec<FileMeta>, [u8; 16], usize, Vec<(String, String)>);

/// Returns `Ok(None)` if more bytes are needed.
fn try_parse_header(buf: &[u8]) -> Result<Option<Header>, String> {
    if buf.len() < HEADER_FIXED + 8 {
        return Ok(None);
    }
    if &buf[..6] != b"SNGPKG" {
        return Err("Not a valid .sng file".into());
    }
    let mut mask = [0u8; 16];
    mask.copy_from_slice(&buf[10..26]);

    let mut c = Cursor { buf, pos: HEADER_FIXED };
    let Some(meta_len) = c.u64() else { return Ok(None) };
    let meta_start = c.pos;
    let Some(meta_end) = meta_start.checked_add(meta_len as usize) else { return Err("Corrupt header".into()) };
    // Need metadata + fileMetaLen
    if buf.len() < meta_end + 8 {
        return Ok(None);
    }
    let meta_count = c.u64().ok_or("Corrupt header")?;
    let mut metadata = Vec::with_capacity(meta_count.min(256) as usize);
    for _ in 0..meta_count {
        let kl = c.i32().ok_or("Corrupt metadata")?.max(0) as usize;
        let k = c.string(kl).ok_or("Corrupt metadata")?;
        let vl = c.i32().ok_or("Corrupt metadata")?.max(0) as usize;
        let v = c.string(vl).ok_or("Corrupt metadata")?;
        metadata.push((k, v));
    }
    c.pos = meta_end;
    let file_meta_len = c.u64().ok_or("Corrupt header")? as usize;
    let fm_start = c.pos;
    let data_start = fm_start + file_meta_len + 8; // + fileDataLen
    if buf.len() < data_start {
        return Ok(None);
    }
    let count = c.u64().ok_or("Corrupt file list")?;
    let mut files = Vec::with_capacity(count.min(1024) as usize);
    for _ in 0..count {
        let nl = c.u8().ok_or("Corrupt file list")? as usize;
        let name = c.string(nl).ok_or("Corrupt file list")?;
        let len = c.u64().ok_or("Corrupt file list")?;
        let index = c.u64().ok_or("Corrupt file list")?;
        if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
            return Err(format!("Refusing unsafe file name in .sng: {name:?}"));
        }
        if name.eq_ignore_ascii_case("song.ini") {
            continue; // generated from metadata instead
        }
        files.push(FileMeta { name, len, index });
    }
    files.sort_by_key(|f| f.index);
    if let Some(first) = files.first()
        && first.index < data_start as u64
    {
        return Err("Corrupt file offsets".into());
    }
    Ok(Some((files, mask, data_start, metadata)))
}

const DEFAULT_METADATA: &[(&str, &str)] = &[
    ("name", "Unknown Name"),
    ("artist", "Unknown Artist"),
    ("album", "Unknown Album"),
    ("genre", "Unknown Genre"),
    ("year", "Unknown Year"),
    ("charter", "Unknown Charter"),
    ("diff_band", "-1"),
    ("diff_guitar", "-1"),
    ("diff_guitar_coop", "-1"),
    ("diff_rhythm", "-1"),
    ("diff_bass", "-1"),
    ("diff_drums", "-1"),
    ("diff_drums_real", "-1"),
    ("diff_keys", "-1"),
    ("diff_guitarghl", "-1"),
    ("diff_guitar_coop_ghl", "-1"),
    ("diff_rhythm_ghl", "-1"),
    ("diff_bassghl", "-1"),
    ("diff_vocals", "-1"),
    ("icon", ""),
    ("loading_phrase", ""),
    ("album_track", "16000"),
    ("playlist_track", "16000"),
    ("playlist", ""),
    ("modchart", "False"),
    ("hopo_frequency", "0"),
    ("eighthnote_hopo", "False"),
    ("multiplier_note", "0"),
    ("video_start_time", "0"),
    ("five_lane_drums", "False"),
    ("pro_drums", "False"),
    ("end_events", "True"),
];

/// Same output as parse-sng's `generateSongIni`.
pub fn generate_ini(metadata: &[(String, String)]) -> String {
    let mut ini = String::from("[song]\n");
    let get = |k: &str| metadata.iter().find(|(mk, _)| mk == k).map(|(_, v)| v.as_str());
    for (key, default) in DEFAULT_METADATA {
        if let Some(v) = get(key)
            && !v.is_empty()
            && v != *default
        {
            ini.push_str(&format!("{key} = {v}\n"));
        }
    }
    for (k, v) in metadata {
        if DEFAULT_METADATA.iter().any(|(dk, _)| dk == k) {
            continue;
        }
        ini.push_str(&format!("{k} = {v}\n"));
    }
    ini
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set TRUSS_TEST_SNG to a real .sng file to run.
    #[test]
    fn extract_matches_across_chunk_sizes() {
        let Ok(path) = std::env::var("TRUSS_TEST_SNG") else { return };
        let data = std::fs::read(path).unwrap();
        let base = std::env::temp_dir().join(format!("truss-test-{}", std::process::id()));
        let a = base.join("whole");
        let b = base.join("chunks");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let mut x = SngExtractor::new(&a);
        x.feed(&data).unwrap();
        assert!(x.is_done());

        let mut y = SngExtractor::new(&b);
        let mut seed = 12345u64;
        let mut rest = &data[..];
        while !rest.is_empty() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let n = ((seed >> 33) % 7000 + 1) as usize;
            let n = n.min(rest.len());
            y.feed(&rest[..n]).unwrap();
            rest = &rest[n..];
        }
        assert!(y.is_done());

        let mut names: Vec<_> = std::fs::read_dir(&a).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        for n in &names {
            let fa = std::fs::read(a.join(n)).unwrap();
            let fb = std::fs::read(b.join(n)).unwrap();
            assert_eq!(fa, fb, "mismatch in {n:?}");
            println!("{:?} {} bytes", n, fa.len());
        }
        for n in &names {
            let f = std::fs::read(a.join(n)).unwrap();
            println!("{:?} starts with {:?}", n, String::from_utf8_lossy(&f[..f.len().min(12)]));
        }
        std::fs::remove_dir_all(&base).unwrap();
    }
}
