//! RhythmVerse access, copied from Truss's rhythmverse.rs (commit 5e53391) and cut
//! down to what the index needs: paging the Clone Hero list, resolving where each chart is
//! hosted, and fetching only a chart's note files (notes.chart / notes.mid and song.ini).
//! Changes from Truss are marked "rv-index".
//!
//! RhythmVerse has no public API, so this uses the JSON endpoint its own website uses. Requests
//! say they come from truss-rv-index (no pretending to be a web browser).

use crate::api::{CLIENT, count};
use futures::{SinkExt, StreamExt, channel::mpsc::Sender};
use serde_json::Value;
use std::path::{Path, PathBuf};

const BASE: &str = "https://rhythmverse.co";

/// RhythmVerse-specific details of a search result.
#[derive(Debug, Clone, Default)]
pub struct RvFile {
    /// Direct download (hosted on RhythmVerse), if any.
    pub download_url: Option<String>,
    /// Where the chart is hosted when RhythmVerse only links to it (e.g. Google Drive).
    pub external_url: Option<String>,
    /// The chart's page on RhythmVerse.
    pub page_url: String,
    /// The song title, for finding the file again when only the page is known.
    pub title: Option<String>,
}
fn absolute(u: &str) -> String {
    if u.starts_with("http://") || u.starts_with("https://") { u.to_string() } else { format!("{BASE}{}", if u.starts_with('/') { u.to_string() } else { format!("/{u}") }) }
}

fn s(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Formats Clone Hero can't load without converting (Rock Band / Guitar Hero console files).
fn needs_conversion(format: &str) -> bool {
    let f = format.to_ascii_lowercase();
    f.starts_with("rb") || f.starts_with("gh") || f.contains("con") || f.contains("xbox") || f.contains("ps3") || f.contains("wii")
}

/// rv-index: one chart of RhythmVerse's Clone Hero list.
#[derive(Debug, Clone, Default)]
pub struct RvEntry {
    pub file_id: String,
    /// RhythmVerse's update_date ("YYYY-MM-DD HH:MM:SS").
    pub modified: String,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub charter: Option<String>,
    pub rv: RvFile,
    /// rv-index: the chart file's size in bytes as RhythmVerse lists it (0 if unknown).
    pub size: u64,
}

/// rv-index: Truss's `to_chart`, keeping only what the index needs.
pub fn to_entry(song: &Value) -> Option<RvEntry> {
    let (data, file) = (&song["data"], &song["file"]);
    let format = s(&file["gameformat"]).unwrap_or_default();
    if needs_conversion(&format) {
        return None;
    }
    let file_id = s(&file["file_id"])?;
    let download = s(&file["download_url"]).filter(|u| !u.eq_ignore_ascii_case("none"));
    let mut external = s(&file["external_url"]);
    // Some charters set the "external" link to the chart's own RhythmVerse page; the file is
    // still on RhythmVerse, at download_file/<uploader>/<id>/<file name>.
    let self_link = external.as_deref().is_some_and(|u| host_kind(u) == Host::RvPage);
    if self_link {
        external = None;
    }
    let direct = || {
        let folder = s(&file["user_folder"]).or_else(|| s(&file["user"]))?;
        let name = s(&file["file_name"])?;
        Some(format!("{BASE}/download_file/{folder}/{file_id}/{name}"))
    };
    // Hosted on RhythmVerse = a download link on rhythmverse.co and no outside host.
    let hosted = download
        .as_deref()
        .map(absolute)
        .filter(|u| u.starts_with(BASE) && external.is_none() && (!self_link || u.contains("/download_file/")))
        .or_else(|| if self_link { direct() } else { None });
    let page = s(&file["file_url_full"]).or_else(|| s(&file["file_url"]).map(|u| absolute(&u))).unwrap_or_else(|| BASE.into());
    let title = s(&data["title"]).or_else(|| s(&file["file_title"]));
    Some(RvEntry {
        modified: s(&file["update_date"]).unwrap_or_default(),
        size: int(&file["size"]).or_else(|| int(&file["file_size"])).unwrap_or(0).max(0) as u64,
        artist: s(&data["artist"]).or_else(|| s(&file["file_artist"])),
        charter: s(&file["charter"]).or_else(|| s(&file["author"]["name"])),
        title: title.clone(),
        rv: RvFile { download_url: hosted, external_url: external, page_url: page, title },
        file_id,
    })
}

/// One page of the whole Clone Hero section by change date (newest or oldest first). Returns the charts Clone Hero
/// can load, the number of raw rows on the page (to detect the end) and the list's total.
pub async fn fetch_list_page(page: u32, records: u32, newest_first: bool) -> Result<(Vec<RvEntry>, usize, u64), String> {
    let form: Vec<(String, String)> = vec![
        ("data_type".into(), "full".into()),
        ("records".into(), records.to_string()),
        ("page".into(), page.to_string()),
        ("sort[0][sort_by]".into(), "update_date".into()),
        ("sort[0][sort_order]".into(), if newest_first { "DESC" } else { "ASC" }.into()),
    ];
    let resp = CLIENT
        .post(format!("{BASE}/api/ch/songfiles/list"))
        // Their endpoint only answers requests marked like this (as their own page sends).
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(90))
        .body(url_encode(&form))
        .send()
        .await
        .map_err(|e| format!("RhythmVerse: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("RhythmVerse returned {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("RhythmVerse: {e}"))?;
    let json: Value = serde_json::from_slice(&bytes).map_err(|_| "RhythmVerse sent an unexpected reply".to_string())?;
    if json["status"] != "success" {
        return Err("RhythmVerse sent an unexpected reply".into());
    }
    let total = int(&json["data"]["records"]["total_filtered"]).unwrap_or(0).max(0) as u64;
    let songs = json["data"]["songs"].as_array().cloned().unwrap_or_default();
    let raw = songs.len();
    Ok((songs.iter().filter_map(to_entry).collect(), raw, total))
}

/// rv-index: looks one chart up by title through RhythmVerse's search (for `scan --id`).
pub async fn search_title(title: &str) -> Result<Vec<RvEntry>, String> {
    let body = url_encode(&[
        ("data_type".into(), "full".into()),
        ("records".into(), "25".into()),
        ("page".into(), "1".into()),
        ("text".into(), title.to_string()),
    ]);
    let bytes = CLIENT
        .post(format!("{BASE}/api/ch/songfiles/search/live"))
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20))
        .body(body)
        .send()
        .await
        .map_err(|e| format!("RhythmVerse: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("RhythmVerse: {e}"))?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| "RhythmVerse sent an unexpected reply".to_string())?;
    Ok(v["data"]["songs"].as_array().into_iter().flatten().filter_map(to_entry).collect())
}

fn url_encode(form: &[(String, String)]) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                b' ' => "+".into(),
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    form.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

/// Percent-encodes the path of a RhythmVerse download link (file names contain spaces etc.).
fn encode_url(u: &str) -> String {
    u.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '[' => "%5B".to_string(),
            ']' => "%5D".to_string(),
            c if c.is_ascii() => c.to_string(),
            c => c.to_string().bytes().map(|b| format!("%{b:02X}")).collect(),
        })
        .collect()
}

/// True if Truss can download this chart (hosted on RhythmVerse, Google Drive or MediaFire).
pub fn can_download(rv: &RvFile) -> bool {
    rv.download_url.is_some() || rv.external_url.as_deref().is_some_and(|u| host_kind(u) != Host::Other)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    DriveFile,
    DriveFolder,
    MediaFire,
    /// The chart's own RhythmVerse page (the file is on RhythmVerse; see `resolve_rv_page`).
    RvPage,
    Other,
}

fn host_kind(u: &str) -> Host {
    let l = u.to_ascii_lowercase();
    if l.contains("drive.google.com") || l.contains("docs.google.com") || l.contains("drive.usercontent.google.com") {
        if l.contains("/folders/") { Host::DriveFolder } else { Host::DriveFile }
    } else if l.contains("mediafire.com") {
        Host::MediaFire
    } else if l.contains("rhythmverse.co/download/") || l.contains("rhythmverse.co/songfile/") {
        Host::RvPage
    } else {
        Host::Other
    }
}

/// True for a link to a chart's own RhythmVerse page (the file is on RhythmVerse).
pub fn is_rv_page(u: &str) -> bool {
    host_kind(u) == Host::RvPage
}

/// The host name shown when a link can't be downloaded in the app.
pub fn host_name(u: &str) -> String {
    u.split("//").nth(1).and_then(|r| r.split('/').next()).unwrap_or(u).trim_start_matches("www.").to_string()
}

fn drive_id(u: &str) -> Option<String> {
    for marker in ["/file/d/", "/folders/", "id="] {
        if let Some(i) = u.find(marker) {
            let id: String = u[i + marker.len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

/// Files a chart's leaderboard hashes come from (lowercase file name).
fn is_note_file(name: &str) -> bool {
    let f = name.rsplit(['/', '\\']).next().unwrap_or(name);
    f.ends_with(".chart") || f.ends_with(".mid") || f.ends_with(".ini")
}

fn is_archive(name: &str) -> bool {
    [".zip", ".7z", ".sng", ".rar"].iter().any(|e| name.ends_with(e))
}

/// Gets just a chart's note files (notes.chart / notes.mid and song.ini) from wherever it's
/// hosted, for working out its leaderboard hashes. Zips are read by byte range and Drive
/// folders file by file, so usually only a few hundred KB are fetched; other archives have to
/// come down whole. Nothing is kept on disk.
pub async fn fetch_note_files(rv: &RvFile) -> Result<Vec<(String, Vec<u8>)>, String> {
    use crate::download::TempGuard;
    // rv-index: a .sng on RhythmVerse is read by byte range too (its header, then the chart).
    if let Some(url) = rv.download_url.as_deref().filter(|u| u.to_ascii_lowercase().ends_with(".sng")) {
        let (meta, mut files) = crate::sng::fetch_remote_notes(&encode_url(url)).await?;
        files.push(("song.ini".into(), crate::sng::generate_ini(&meta).into_bytes()));
        return Ok(files);
    }
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!("truss-notes-{}-{n}", std::process::id()));
    let _guard = TempGuard(Some(work.clone()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let unpacked = work.join("chart");
    // Progress goes nowhere: dropping the receiver makes every send a no-op.
    let (mut tx, _) = futures::channel::mpsc::channel(1);

    let archive = async |url: &str, tx: &mut Sender<crate::download::Progress>| -> Result<(), String> {
        let keep = |n: &str| safe_rel(n, false).filter(|r| is_note_file(&r.to_string_lossy().to_ascii_lowercase()));
        if crate::zip_ranges::fetch_selected(url, &unpacked, keep, tx).await? {
            return Ok(());
        }
        // rv-index: the server ignores ranges (RhythmVerse's own links): read the zip from the
        // start and stop once the note files are in.
        let _ = std::fs::remove_dir_all(&unpacked);
        match crate::zip_stream::fetch(url, &work, &unpacked, keep).await? {
            crate::zip_stream::Streamed::Unpacked => Ok(()),
            crate::zip_stream::Streamed::File(file) => unpack_any(file, &unpacked, true).await,
            crate::zip_stream::Streamed::Unsupported => {
                let _ = std::fs::remove_dir_all(&unpacked);
                let file = fetch_to_dir(url, &work, "chart.bin", tx).await?;
                unpack_any(file, &unpacked, true).await
            }
        }
    };
    match (&rv.download_url, rv.external_url.as_deref().map(|u| (u, host_kind(u)))) {
        (Some(url), _) => archive(&encode_url(url), &mut tx).await?,
        (None, Some((u, Host::DriveFile))) => {
            let id = drive_id(u).ok_or("Couldn't read the Google Drive link")?;
            archive(&format!("https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"), &mut tx).await?
        }
        (None, Some((u, Host::DriveFolder))) => {
            let id = drive_id(u).ok_or("Couldn't read the Google Drive link")?;
            download_drive_folder(&id, &unpacked, false, 0, is_note_file, &mut tx).await?;
            // Some folders hold an archive rather than loose chart files.
            if find_chart_folder(&unpacked).is_none() {
                let _ = std::fs::remove_dir_all(&unpacked);
                let stash = work.join("archive");
                download_drive_folder(&id, &stash, false, 0, is_archive, &mut tx).await?;
                let a = first_archive(&stash).ok_or("No Clone Hero chart was found in the Google Drive folder.")?;
                unpack_any(a, &unpacked, true).await?;
            }
        }
        (None, Some((u, Host::MediaFire))) => archive(&mediafire_link(u).await?, &mut tx).await?,
        (None, Some((u, Host::RvPage))) => archive(&encode_url(&resolve_rv_page(u, rv.title.as_deref()).await?), &mut tx).await?,
        (None, Some((u, Host::Other))) => return Err(format!("This chart is hosted on {}, which Truss can't read.", host_name(u))),
        (None, None) => return Err("RhythmVerse has no download for this chart.".into()),
    }
    let folder = find_chart_folder(&unpacked).ok_or("No Clone Hero chart was found in the download.")?;
    let mut files = Vec::new();
    for e in std::fs::read_dir(&folder).map_err(|e| e.to_string())?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.file_type().is_ok_and(|t| t.is_file()) && is_note_file(&name.to_ascii_lowercase()) {
            files.push((name, std::fs::read(e.path()).map_err(|e| e.to_string())?));
        }
    }
    Ok(files)
}

/// Streams `url` into `dir`, named from the server's file name (or `fallback`). Returns the file.
async fn fetch_to_dir(url: &str, dir: &Path, fallback: &str, tx: &mut Sender<crate::download::Progress>) -> Result<PathBuf, String> {
    use crate::download::Progress;
    use std::io::Write;
    let resp = CLIENT
        .get(url)
        .timeout(std::time::Duration::from_secs(60 * 30))
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            404 | 410 => "This chart is no longer available: its download link is dead (the file was deleted or moved). Try another version of this song.".into(),
            401 | 403 => "The host refused the download (the file may be private). Open the link in your browser.".into(),
            429 => "The host is limiting downloads right now. Wait a bit and try again.".into(),
            c => format!("Download failed: the host returned {c}"),
        });
    }
    let headers = resp.headers();
    if headers.get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/html")) {
        return Err("Google Drive didn't hand over the file (it may be over its download limit or need a sign-in). Open the link in your browser.".into());
    }
    let name = headers
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .and_then(file_name_from_disposition)
        .map(|n| crate::download::sanitize_filename(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    let total = resp.content_length();
    let path = dir.join(name);
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).map_err(|e| format!("Cannot write to library: {e}"))?);
    let mut stream = resp.bytes_stream();
    let mut done = 0u64;
    let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Connection lost: {e}"))?;
        count(chunk.len());
        done += chunk.len() as u64;
        out.write_all(&chunk).map_err(|e| format!("Write failed: {e}"))?;
        if last.elapsed() >= std::time::Duration::from_millis(33) {
            last = std::time::Instant::now();
            let _ = tx.try_send(Progress::Bytes { done, total });
        }
    }
    let _ = tx.send(Progress::Bytes { done, total }).await;
    out.flush().map_err(|e| format!("Write failed: {e}"))?;
    if let Some(t) = total
        && done < t
    {
        return Err(format!(
            "The download was cut off by the server ({:.0} of {:.0} MB). Try again.",
            done as f64 / 1_048_576.0,
            t as f64 / 1_048_576.0
        ));
    }
    Ok(path)
}

fn file_name_from_disposition(cd: &str) -> Option<String> {
    if let Some(i) = cd.find("filename*=") {
        let v = cd[i + 10..].split(';').next()?.trim().trim_matches('"');
        let v = v.strip_prefix("UTF-8''").or_else(|| v.strip_prefix("utf-8''")).unwrap_or(v);
        return Some(percent_decode(v));
    }
    let i = cd.find("filename=")?;
    Some(cd[i + 9..].split(';').next()?.trim().trim_matches('"').to_string())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lists a public Google Drive folder by reading its web page (there's no other way without a
/// Google API key). Files are matched to their names by order on the page.
async fn list_drive_folder(id: &str) -> Result<Vec<(String, String, bool)>, String> {
    let html = CLIENT
        .get(format!("https://drive.google.com/drive/folders/{id}"))
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| host_error(&e, "Google Drive"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    count(html.len());
    // Positions of each distinct data-id="..." in page order.
    let mut groups: Vec<(String, usize)> = Vec::new();
    let mut at = 0;
    while let Some(i) = html[at..].find("data-id=\"") {
        let start = at + i + 9;
        let Some(len) = html[start..].find('"') else { break };
        let found = &html[start..start + len];
        if found != "_gd" && groups.last().is_none_or(|g| g.0 != found) {
            groups.push((found.to_string(), at + i));
        }
        at = start + len;
    }
    let mut entries = Vec::new();
    for (n, (fid, pos)) in groups.iter().enumerate() {
        let end = groups.get(n + 1).map(|g| g.1).unwrap_or(html.len());
        let slice = &html[*pos..end];
        let label = slice.find("aria-label=\"").and_then(|i| {
            let s = &slice[i + 12..];
            s.find('"').map(|e| html_unescape(&s[..e]))
        });
        let label = label.unwrap_or_else(|| fid.clone());
        // Labels look like "song.ini" or "song.ini Text"; folders have no extension.
        let name = label
            .split_whitespace()
            .scan(String::new(), |acc, w| {
                if !acc.is_empty() {
                    acc.push(' ');
                }
                acc.push_str(w);
                Some(acc.clone())
            })
            .filter(|cand| has_extension(cand))
            .last()
            .unwrap_or(label.clone());
        let is_folder = !has_extension(&name);
        let name = if is_folder {
            label.trim_end_matches(" Shared folder").trim_end_matches(" Folder").to_string()
        } else {
            name
        };
        entries.push((fid.clone(), crate::download::sanitize_filename(&name), is_folder));
    }
    if entries.is_empty() {
        return Err("The Google Drive folder is empty or not public. Open the link in your browser.".into());
    }
    Ok(entries)
}

fn has_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && (2..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">")
}

fn download_drive_folder<'a>(
    id: &'a str,
    dest: &'a Path,
    keep_video: bool,
    depth: u32,
    want: fn(&str) -> bool,
    tx: &'a mut Sender<crate::download::Progress>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        use crate::download::Progress;
        if depth > 3 {
            return Ok(());
        }
        let entries = list_drive_folder(id).await?;
        std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        let files: Vec<_> = entries.iter().filter(|e| !e.2).collect();
        for (n, (fid, name, _)) in files.iter().enumerate() {
            let lower = name.to_ascii_lowercase();
            if lower == "desktop.ini" || (!keep_video && lower.starts_with("video.")) || !want(&lower) {
                continue;
            }
            let _ = tx.send(Progress::Status(format!("Downloading {name} ({}/{})", n + 1, files.len()))).await;
            let url = format!("https://drive.usercontent.google.com/download?id={fid}&export=download&confirm=t");
            let got = fetch_to_dir(&url, dest, name, tx).await?;
            // Keep Drive's listed name (the server's may differ in case/characters).
            let want = dest.join(name);
            if got != want {
                let _ = std::fs::rename(&got, &want);
            }
        }
        for (fid, name, _) in entries.iter().filter(|e| e.2) {
            download_drive_folder(fid, &dest.join(name), keep_video, depth + 1, want, tx).await?;
        }
        Ok(())
    })
}

/// A plain message for a failed request to a chart's host: a missing file means the chart is
/// gone, not that something went wrong in Truss.
fn host_error(e: &reqwest::Error, host: &str) -> String {
    match e.status().map(|s| s.as_u16()) {
        Some(404 | 410) => format!(
            "This chart is no longer available: its {host} link is dead (the files were deleted or moved). Try another version of this song."
        ),
        Some(401 | 403) => format!(
            "This chart isn't available: its {host} files are private or were removed. Open the link in your browser to check."
        ),
        Some(429) => format!("{host} is limiting downloads right now. Wait a bit and try again."),
        Some(code) => format!("{host} returned an error ({code}). Try again later."),
        None => format!("Couldn't reach {host}. Check your connection and try again."),
    }
}

/// The direct download link of a chart whose link is its own RhythmVerse page: RhythmVerse's
/// search (by title) has the uploader folder and file name, matched on the file ID in the link.
async fn resolve_rv_page(page: &str, title: Option<&str>) -> Result<String, String> {
    let id: String = page
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    let not_found = || "Couldn't find this chart's file on RhythmVerse. Open its RhythmVerse page to download it.".to_string();
    let title = title.filter(|t| !t.trim().is_empty()).ok_or_else(not_found)?;
    let body = url_encode(&[
        ("data_type".into(), "full".into()),
        ("records".into(), "25".into()),
        ("page".into(), "1".into()),
        ("text".into(), title.to_string()),
    ]);
    let v: Value = CLIENT
        .post(format!("{BASE}/api/ch/songfiles/search/live"))
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20))
        .body(body)
        .send()
        .await
        .map_err(|_| "Couldn't reach RhythmVerse. Check your connection and try again.".to_string())?
        .bytes()
        .await
        .map_err(|_| not_found())
        .and_then(|b| {
            count(b.len());
            serde_json::from_slice(&b).map_err(|_| not_found())
        })?;
    let songs = v["data"]["songs"].as_array().cloned().unwrap_or_default();
    let file = songs.iter().map(|s| &s["file"]).find(|f| s(&f["file_id"]).as_deref() == Some(id.as_str())).ok_or_else(not_found)?;
    let folder = s(&file["user_folder"]).or_else(|| s(&file["user"])).ok_or_else(not_found)?;
    let name = s(&file["file_name"]).ok_or_else(not_found)?;
    Ok(format!("{BASE}/download_file/{folder}/{id}/{name}"))
}

/// MediaFire shows a page with the real download link on it.
async fn mediafire_link(page: &str) -> Result<String, String> {
    let html = CLIENT
        .get(page)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| host_error(&e, "MediaFire"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    count(html.len());
    let mut at = 0;
    while let Some(i) = html[at..].find("download") {
        let pos = at + i;
        // Look for "https://downloadNNNN.mediafire.com/..." around here.
        let start = html[..pos].rfind("https://").filter(|s| pos - s < 12);
        if let Some(s) = start {
            let end = html[s..].find(['"', '\'', ' ', '<']).map(|e| s + e).unwrap_or(html.len());
            let link = html_unescape(&html[s..end]);
            if link.contains(".mediafire.com/") && link.split('/').nth(2).is_some_and(|h| h.starts_with("download")) {
                return Ok(link);
            }
        }
        at = pos + 8;
    }
    Err("Couldn't find the MediaFire download link. Open the link in your browser.".into())
}

fn first_archive(dir: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| ["zip", "7z", "sng"].contains(&e.to_string_lossy().to_ascii_lowercase().as_str())))
}

/// Unpacks a downloaded .zip / .7z (or places a .sng's files) into `dest`.
async fn unpack_any(file: PathBuf, dest: &Path, notes_only: bool) -> Result<(), String> {
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let mut magic = [0u8; 6];
        {
            use std::io::Read;
            let _ = std::fs::File::open(&file).and_then(|mut f| f.read(&mut magic));
        }
        // The file's first bytes decide; the name is only a fallback (a download saved under a
        // made-up name like "chart.zip" can really be a .7z).
        let ext = file.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let kind = if magic.starts_with(b"PK") {
            "zip"
        } else if magic.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
            "7z"
        } else if magic.starts_with(b"SNGPKG") {
            "sng"
        } else if magic.starts_with(b"Rar!") {
            "rar"
        } else {
            ext.as_str()
        };
        match kind {
            "zip" => unzip(&file, &dest, notes_only),
            "7z" => un7z(&file, &dest, notes_only),
            "sng" => unsng(&file, &dest),
            "rar" => unrar_all(&file, &dest, notes_only),
            _ => Err(format!("Truss doesn't know how to open \"{}\".", file.file_name().unwrap_or_default().to_string_lossy())),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A safe relative path for an archive entry (no "..", no absolute paths), or `None` to skip it.
fn safe_rel(name: &str, keep_video: bool) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            p if p.contains(':') => return None,
            "__MACOSX" => return None,
            p => out.push(p),
        }
    }
    let file = out.file_name()?.to_string_lossy().to_ascii_lowercase();
    if file == "desktop.ini" || (!keep_video && file.starts_with("video.")) {
        return None;
    }
    Some(out)
}

/// rv-index: an archive entry to unpack: with `notes_only`, just note files and song.ini.
fn keep_entry(name: &str, notes_only: bool) -> Option<PathBuf> {
    safe_rel(name, false).filter(|r| !notes_only || is_note_file(&r.to_string_lossy().to_ascii_lowercase()))
}

fn write_entry(dest: &Path, rel: &Path, reader: &mut dyn std::io::Read) -> Result<(), String> {
    let out = dest.join(rel);
    if let Some(p) = out.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).map_err(|e| format!("Couldn't unpack: {e}"))?);
    std::io::copy(reader, &mut w).map_err(|e| format!("Couldn't unpack: {e}"))?;
    Ok(())
}

fn unzip(file: &Path, dest: &Path, notes_only: bool) -> Result<(), String> {
    let f = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("The download isn't a valid .zip: {e}"))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("Damaged .zip: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map(|n| n.into_owned()).unwrap_or_default();
        let Some(rel) = keep_entry(&name, notes_only) else { continue };
        write_entry(dest, &rel, &mut entry)?;
    }
    Ok(())
}

fn un7z(file: &Path, dest: &Path, notes_only: bool) -> Result<(), String> {
    let mut reader = sevenz_rust2::ArchiveReader::open(file, sevenz_rust2::Password::empty())
        .map_err(|e| format!("The download isn't a valid .7z: {e}"))?;
    let mut err = None;
    reader
        .for_each_entries(|entry, data| {
            if entry.is_directory() {
                return Ok(true);
            }
            if let Some(rel) = keep_entry(entry.name(), notes_only)
                && let Err(e) = write_entry(dest, &rel, data)
            {
                err = Some(e);
                return Ok(false);
            }
            Ok(true)
        })
        .map_err(|e| format!("Couldn't unpack the .7z: {e}"))?;
    err.map_or(Ok(()), Err)
}

fn unrar_all(file: &Path, dest: &Path, notes_only: bool) -> Result<(), String> {
    let mut archive = unrar::Archive::new(file)
        .open_for_processing()
        .map_err(|e| format!("The download isn't a valid .rar: {e}"))?;
    while let Some(header) = archive.read_header().map_err(|e| format!("Couldn't unpack the .rar: {e}"))? {
        let (is_file, name) = {
            let entry = header.entry();
            (entry.is_file(), entry.filename.to_string_lossy().into_owned())
        };
        archive = match keep_entry(&name, notes_only).filter(|_| is_file) {
            Some(rel) => {
                let (data, rest) = header.read().map_err(|e| format!("Couldn't unpack the .rar: {e}"))?;
                write_entry(dest, &rel, &mut data.as_slice())?;
                rest
            }
            None => header.skip().map_err(|e| format!("Couldn't unpack the .rar: {e}"))?,
        };
    }
    Ok(())
}

fn unsng(file: &Path, dest: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut x = crate::sng::SngExtractor::new(dest);
    let mut f = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        x.feed(&buf[..n])?;
    }
    if x.is_done() { Ok(()) } else { Err("The .sng file is incomplete".into()) }
}

fn find_chart_folder(dir: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_ascii_lowercase();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            subdirs.push(e.path());
        } else if n == "song.ini" || n == "notes.chart" || n == "notes.mid" {
            return Some(dir.to_path_buf());
        }
    }
    subdirs.into_iter().find_map(|d| find_chart_folder(&d))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_results() {
        let song: Value = serde_json::from_str(r#"{
            "data": {"title": "Home", "artist": "Hue", "album": "A", "year": 2026, "song_length": 205, "diff_guitar": "3", "diff_keys": "0"},
            "file": {"file_id": "12e0e0a7", "gameformat": "rv", "download_url": "/download_file/x/12e0e0a7/HUE - Home.zip",
                     "file_url_full": "https://rhythmverse.co/songfile/12e0e0a7", "album_art": "/assets/album_art/x/a.png",
                     "author": {"name": "Miggy_RG"}, "diff_drums": 5, "update_date": "2026-11-08 01:33:03"}
        }"#).unwrap();
        let c = to_entry(&song).unwrap();
        assert_eq!(c.title.as_deref(), Some("Home"));
        assert_eq!(c.charter.as_deref(), Some("Miggy_RG"));
        assert_eq!(c.modified, "2026-11-08 01:33:03");
        assert_eq!(c.rv.download_url.as_deref(), Some("https://rhythmverse.co/download_file/x/12e0e0a7/HUE - Home.zip"));
        assert_eq!(encode_url(c.rv.download_url.as_deref().unwrap()), "https://rhythmverse.co/download_file/x/12e0e0a7/HUE%20-%20Home.zip");

        let ext: Value = serde_json::from_str(r#"{"data": {"title": "T"}, "file": {"file_id": "1", "gameformat": "ch", "download_url": "https://drive.google.com/file/d/x", "external_url": "https://drive.google.com/file/d/x"}}"#).unwrap();
        let e = to_entry(&ext).unwrap();
        assert!(e.rv.download_url.is_none() && e.rv.external_url.is_some());
        let con: Value = serde_json::from_str(r#"{"data": {}, "file": {"file_id": "2", "gameformat": "rb3xbox"}}"#).unwrap();
        assert!(to_entry(&con).is_none());

        // A chart whose "external" link is its own RhythmVerse page is hosted on RhythmVerse.
        let own: Value = serde_json::from_str(r#"{"data": {"title": "Shared Trauma"}, "file": {"file_id": "e0178", "gameformat": "ch",
            "download_url": "https://rhythmverse.co/download/e0178", "external_url": "https://rhythmverse.co/download/e0178",
            "user_folder": "someone", "file_name": "Pierce The Veil - Shared Trauma.zip"}}"#).unwrap();
        let rv = to_entry(&own).unwrap().rv;
        assert_eq!(rv.download_url.as_deref(), Some("https://rhythmverse.co/download_file/someone/e0178/Pierce The Veil - Shared Trauma.zip"));
        assert!(rv.external_url.is_none() && can_download(&rv));
    }

    #[test]
    fn keeps_only_note_files() {
        assert!(keep_entry("Song/notes.mid", true).is_some());
        assert!(keep_entry("Song/song.ini", true).is_some());
        assert!(keep_entry("Song/song.ogg", true).is_none());
        assert!(keep_entry("Song/song.ogg", false).is_some());
        assert!(keep_entry("Song/video.mp4", false).is_none());
    }

    #[test]
    fn links_and_paths() {
        assert_eq!(drive_id("https://drive.google.com/file/d/1QjXY-C_h/view?usp=sharing").as_deref(), Some("1QjXY-C_h"));
        assert_eq!(drive_id("https://drive.google.com/drive/u/0/folders/1b5Bq?usp=x").as_deref(), Some("1b5Bq"));
        assert_eq!(drive_id("https://drive.google.com/open?id=abc_9").as_deref(), Some("abc_9"));
        assert_eq!(host_kind("https://drive.google.com/drive/folders/x"), Host::DriveFolder);
        assert_eq!(host_kind("https://www.mediafire.com/file/x/y.zip/file"), Host::MediaFire);
        assert_eq!(host_kind("https://mega.nz/file/x"), Host::Other);
        assert_eq!(host_name("https://www.mediafire.com/file/x"), "mediafire.com");
        assert_eq!(safe_rel("Song/notes.chart", true), Some(PathBuf::from("Song").join("notes.chart")));
        assert_eq!(safe_rel("../evil.txt", true), None);
        assert_eq!(safe_rel("C:/evil.txt", true), None);
        assert_eq!(safe_rel("Song/video.mp4", false), None);
        assert_eq!(safe_rel("__MACOSX/._x", true), None);
        assert!(has_extension("song.ini") && has_extension("notes.chart") && !has_extension("Everlong (Foo Fighters)"));
        assert_eq!(file_name_from_disposition("attachment; filename=\"Foo - Bar.7z\"").as_deref(), Some("Foo - Bar.7z"));
        assert_eq!(file_name_from_disposition("attachment; filename*=UTF-8''A%20B.zip").as_deref(), Some("A B.zip"));
    }

    /// Bladee - Be Nice 2 Me: its Google Drive folder was deleted.
    #[tokio::test]
    #[ignore = "network"]
    async fn live_dead_drive_folder() {
        let e = list_drive_folder("1J4GnVBUjqGvalft84HSjq1K_eH0szaaU").await.unwrap_err();
        assert!(e.starts_with("This chart is no longer available"), "{e}");
    }

}
