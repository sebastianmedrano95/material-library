use crc32fast::Hasher as Crc32;
use pbr_material_indexer::{scan_with_progress, Material, ScanProgress, ScanReport};
use serde_json::{json, Value};
use std::collections::{hash_map::DefaultHasher, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_BODY: usize = 256 * 1024;

struct Event {
    timestamp_ms: u128,
    source: String,
    level: String,
    message: String,
}

#[derive(Default)]
struct LibraryState {
    root: String,
    materials: Vec<Material>,
    texture_paths: HashSet<PathBuf>,
    scanning: bool,
    scan_mode_selected: bool,
    preview_mode: String,
    thumbnailing: bool,
    thumbnails_done: usize,
    thumbnails_total: usize,
    thumbnails_ready: HashSet<PathBuf>,
    thumbnails_failed: HashSet<PathBuf>,
    thumbnails_active: HashSet<String>,
    files_examined: usize,
    entries_seen: usize,
    folders_seen: usize,
    folders_skipped: usize,
    status: String,
    events: VecDeque<Event>,
    favorites: HashSet<String>,
}

struct AppRuntime {
    state: Mutex<LibraryState>,
    shutdown: AtomicBool,
    token: String,
    favorites_path: Option<PathBuf>,
    vips_program: Option<PathBuf>,
}

impl AppRuntime {
    fn new(token: String) -> Self {
        let favorites_path = favorites_path();
        let favorites = favorites_path
            .as_deref()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|contents| serde_json::from_str::<HashSet<String>>(&contents).ok())
            .unwrap_or_default();
        Self {
            state: Mutex::new(LibraryState {
                status: "Choose a folder to get started.".to_owned(),
                preview_mode: "pure".to_owned(),
                favorites,
                ..Default::default()
            }),
            shutdown: AtomicBool::new(false),
            token,
            favorites_path,
            vips_program: find_vipsthumbnail(),
        }
    }

    fn record(&self, source: &str, level: &str, message: impl Into<String>) {
        let message = message.into();
        if let Ok(mut state) = self.state.lock() {
            state.status = message.clone();
            state.events.push_back(Event {
                timestamp_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                source: source.to_owned(),
                level: level.to_owned(),
                message,
            });
            while state.events.len() > 500 {
                state.events.pop_front();
            }
        }
    }

    fn authorized_path(&self, path: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(path);
        let state = self
            .state
            .lock()
            .map_err(|_| "Application state unavailable")?;
        state
            .texture_paths
            .contains(&path)
            .then_some(path)
            .ok_or_else(|| {
                "The requested path is not part of the current material index".to_owned()
            })
    }
}

fn favorites_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        Some(
            PathBuf::from(base)
                .join("MaterialLibrary")
                .join("favorites.json"),
        )
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("MaterialLibrary")
                .join("favorites.json"),
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("material-library").join("favorites.json"))
    }
}

fn find_vipsthumbnail() -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "vipsthumbnail.exe"
    } else {
        "vipsthumbnail"
    };
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("MATERIAL_LIBRARY_VIPS") {
        let path = PathBuf::from(configured);
        if path.is_dir() {
            candidates.push(path.join(executable));
            candidates.push(path.join("bin").join(executable));
            if let Ok(children) = fs::read_dir(&path) {
                candidates.extend(
                    children
                        .flatten()
                        .map(|entry| entry.path().join("bin").join(executable)),
                );
            }
        } else {
            candidates.push(path);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|directory| directory.join(executable)));
    }
    #[cfg(windows)]
    if let Some(local_data) = std::env::var_os("LOCALAPPDATA") {
        let path = PathBuf::from(local_data)
            .join("MaterialLibrary")
            .join("tools")
            .join("libvips");
        candidates.push(path.join(executable));
        candidates.push(path.join("bin").join(executable));
        if let Ok(children) = fs::read_dir(&path) {
            candidates.extend(
                children
                    .flatten()
                    .map(|entry| entry.path().join("bin").join(executable)),
            );
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("MaterialLibrary")
                .join("tools")
                .join("libvips")
                .join("bin")
                .join(executable),
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("material-library")
                .join("tools")
                .join("libvips")
                .join("bin")
                .join(executable),
        );
    }
    candidates.into_iter().find(|candidate| {
        candidate.is_file()
            && Command::new(candidate)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
    })
}

fn thumbnail_cache_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA"))?;
        Some(
            PathBuf::from(base)
                .join("MaterialLibrary")
                .join("cache")
                .join("thumbnails"),
        )
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("MaterialLibrary")
                .join("thumbnails"),
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
        Some(base.join("material-library").join("thumbnails"))
    }
}

fn thumbnail_cache_path(source: &Path) -> io::Result<PathBuf> {
    let metadata = fs::metadata(source)?;
    let modified = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut hasher = DefaultHasher::new();
    source.to_string_lossy().hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    modified.hash(&mut hasher);
    let root = thumbnail_cache_root()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Cache directory unavailable"))?;
    Ok(root.join(format!("{:016x}.webp", hasher.finish())))
}

fn thumbnail_dimensions_path(cache: &Path) -> PathBuf {
    cache.with_extension("json")
}

fn read_cached_dimensions(cache: &Path) -> Option<pbr_material_indexer::ImageDimensions> {
    let metadata: Value =
        serde_json::from_slice(&fs::read(thumbnail_dimensions_path(cache)).ok()?).ok()?;
    Some(pbr_material_indexer::ImageDimensions {
        width: usize::try_from(metadata.get("width")?.as_u64()?).ok()?,
        height: usize::try_from(metadata.get("height")?.as_u64()?).ok()?,
    })
}

fn write_favorites(path: &Path, favorites: &HashSet<String>) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid favorites path"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(favorites).map_err(io::Error::other)?;
    fs::write(&temporary, bytes)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if path.exists() => {
            fs::remove_file(path)?;
            fs::rename(&temporary, path).map_err(|_| error)
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error)
        }
    }
}

fn save_favorites(app: &AppRuntime, favorites: HashSet<String>) -> Result<(), String> {
    let path = app
        .favorites_path
        .as_deref()
        .ok_or("Could not locate a user configuration folder for favorites")?;
    write_favorites(path, &favorites)
        .map_err(|error| format!("Could not save favorites: {error}"))?;
    let mut state = app
        .state
        .lock()
        .map_err(|_| "Application state unavailable")?;
    state.favorites = favorites;
    Ok(())
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    let marker = b"\r\n\r\n";
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes
            .windows(marker.len())
            .position(|window| window == marker)
        {
            break index + marker.len();
        }
        if bytes.len() > 64 * 1024 {
            return Err("HTTP header is too large".to_owned());
        }
        let mut chunk = [0u8; 8192];
        let count = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("Connection closed before request headers".to_owned());
        }
        bytes.extend_from_slice(&chunk[..count]);
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = head.lines();
    let mut first = lines.next().ok_or("Empty request")?.split_whitespace();
    let method = first.next().ok_or("Missing HTTP method")?.to_owned();
    let target = first.next().ok_or("Missing request path")?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    let body_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if body_length > MAX_BODY {
        return Err("Request body is too large".to_owned());
    }
    while bytes.len() - header_end < body_length {
        let mut chunk = [0u8; 8192];
        let count = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("Incomplete request body".to_owned());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(Request {
        method,
        target,
        headers,
        body: bytes[header_end..header_end + body_length].to_vec(),
    })
}

fn respond(stream: &mut TcpStream, status: u16, mime: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

fn respond_json(stream: &mut TcpStream, status: u16, value: Value) {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    respond(stream, status, "application/json; charset=utf-8", &bytes);
}

struct TempArchive {
    path: PathBuf,
    length: u64,
    count: usize,
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn zip_write(file: &mut File, offset: &mut u64, bytes: &[u8]) -> Result<(), String> {
    if offset.saturating_add(bytes.len() as u64) > u32::MAX as u64 {
        return Err("The ZIP exceeds the classic format's 4 GiB limit.".to_owned());
    }
    file.write_all(bytes)
        .map_err(|error| format!("Could not write ZIP archive: {error}"))?;
    *offset += bytes.len() as u64;
    Ok(())
}

fn zip_u16(file: &mut File, offset: &mut u64, value: u16) -> Result<(), String> {
    zip_write(file, offset, &value.to_le_bytes())
}

fn zip_u32(file: &mut File, offset: &mut u64, value: u32) -> Result<(), String> {
    zip_write(file, offset, &value.to_le_bytes())
}

fn zip_entry_name(root: &Path, source: &Path) -> Result<Vec<u8>, String> {
    let relative = source
        .strip_prefix(root)
        .map_err(|_| "A texture is outside the indexed folder".to_owned())?;
    let name = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if name.is_empty() {
        return Err("Could not construct a ZIP entry name for a texture".to_owned());
    }
    let bytes = name.into_bytes();
    if bytes.len() > u16::MAX as usize {
        return Err("La ruta de una textura es demasiado larga para el ZIP".to_owned());
    }
    Ok(bytes)
}

fn create_material_archive(app: &AppRuntime, requested: &[usize]) -> Result<TempArchive, String> {
    if requested.is_empty() {
        return Err("Select at least one material to download.".to_owned());
    }
    let requested: HashSet<usize> = requested.iter().copied().collect();
    let (root, paths, found_count) = {
        let state = app
            .state
            .lock()
            .map_err(|_| "Application state unavailable")?;
        let root = PathBuf::from(&state.root);
        let mut paths = Vec::new();
        let mut found = 0usize;
        for (index, material) in state.materials.iter().enumerate() {
            if requested.contains(&index) {
                found += 1;
                paths.extend(material.textures.iter().map(|texture| texture.path.clone()));
            }
        }
        (root, paths, found)
    };
    if found_count != requested.len() {
        return Err("The selection changed. Select the materials again.".to_owned());
    }
    let mut unique = HashSet::new();
    let mut paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| unique.insert(path.clone()))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err("The selected materials have no maps to download.".to_owned());
    }
    if paths.len() > u16::MAX as usize {
        return Err("Classic ZIP supports up to 65,535 textures per download.".to_owned());
    }

    let unique_name = format!(
        "MaterialLibrary-{}-{}.zip",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let archive_path = std::env::temp_dir().join(unique_name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive_path)
        .map_err(|error| format!("Could not create temporary download archive: {error}"))?;
    let mut archive = TempArchive {
        path: archive_path,
        length: 0,
        count: paths.len(),
    };
    let mut offset = 0u64;
    let mut central = Vec::with_capacity(paths.len());
    const FLAGS: u16 = 0x0808; // UTF-8 names + data descriptor (streaming)
    const DOS_DATE: u16 = 0x0021; // 1980-01-01
    let mut buffer = [0u8; 64 * 1024];

    for source in &paths {
        let name = zip_entry_name(&root, source)?;
        let local_offset = u32::try_from(offset).map_err(|_| "El ZIP supera 4 GB".to_owned())?;
        zip_u32(&mut file, &mut offset, 0x0403_4b50)?;
        zip_u16(&mut file, &mut offset, 20)?;
        zip_u16(&mut file, &mut offset, FLAGS)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, DOS_DATE)?;
        zip_u32(&mut file, &mut offset, 0)?;
        zip_u32(&mut file, &mut offset, 0)?;
        zip_u32(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, name.len() as u16)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_write(&mut file, &mut offset, &name)?;

        app.record(
            "DOWNLOAD",
            "info",
            format!("Incluyendo textura: {}", source.display()),
        );
        let mut input = File::open(source)
            .map_err(|error| format!("Could not open {}: {error}", source.display()))?;
        let mut hasher = Crc32::new();
        let mut size = 0u64;
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
            if count == 0 {
                break;
            }
            size = size.saturating_add(count as u64);
            if size > u32::MAX as u64 {
                return Err(format!(
                    "{} exceeds the classic ZIP 4 GiB limit",
                    source.display()
                ));
            }
            hasher.update(&buffer[..count]);
            zip_write(&mut file, &mut offset, &buffer[..count])?;
        }
        let crc = hasher.finalize();
        let size = size as u32;
        zip_u32(&mut file, &mut offset, 0x0807_4b50)?;
        zip_u32(&mut file, &mut offset, crc)?;
        zip_u32(&mut file, &mut offset, size)?;
        zip_u32(&mut file, &mut offset, size)?;
        central.push((name, crc, size, local_offset));
    }

    let central_offset = u32::try_from(offset).map_err(|_| "El ZIP supera 4 GB".to_owned())?;
    for (name, crc, size, local_offset) in &central {
        zip_u32(&mut file, &mut offset, 0x0201_4b50)?;
        zip_u16(&mut file, &mut offset, 20)?;
        zip_u16(&mut file, &mut offset, 20)?;
        zip_u16(&mut file, &mut offset, FLAGS)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, DOS_DATE)?;
        zip_u32(&mut file, &mut offset, *crc)?;
        zip_u32(&mut file, &mut offset, *size)?;
        zip_u32(&mut file, &mut offset, *size)?;
        zip_u16(&mut file, &mut offset, name.len() as u16)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u16(&mut file, &mut offset, 0)?;
        zip_u32(&mut file, &mut offset, 0)?;
        zip_u32(&mut file, &mut offset, *local_offset)?;
        zip_write(&mut file, &mut offset, name)?;
    }
    let central_size = offset
        .checked_sub(central_offset as u64)
        .ok_or("ZIP central directory offset invalid")? as u32;
    zip_u32(&mut file, &mut offset, 0x0605_4b50)?;
    zip_u16(&mut file, &mut offset, 0)?;
    zip_u16(&mut file, &mut offset, 0)?;
    zip_u16(&mut file, &mut offset, central.len() as u16)?;
    zip_u16(&mut file, &mut offset, central.len() as u16)?;
    zip_u32(&mut file, &mut offset, central_size)?;
    zip_u32(&mut file, &mut offset, central_offset)?;
    zip_u16(&mut file, &mut offset, 0)?;
    file.flush()
        .map_err(|error| format!("Could not finish ZIP archive: {error}"))?;
    archive.length = offset;
    Ok(archive)
}

fn respond_archive(stream: &mut TcpStream, archive: &TempArchive) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Disposition: attachment; filename=\"MaterialLibrary-textures.zip\"\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        archive.length
    );
    stream.write_all(header.as_bytes())?;
    let mut file = File::open(&archive.path)?;
    io::copy(&mut file, stream)?;
    Ok(())
}

fn json_body(request: &Request) -> Result<Value, String> {
    serde_json::from_slice(&request.body).map_err(|error| error.to_string())
}

fn request_path(target: &str) -> &str {
    target.split('?').next().unwrap_or("/")
}

fn query_value(target: &str, key: &str) -> Result<String, String> {
    let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == key {
            return percent_decode(value);
        }
    }
    Err(format!("Missing query parameter: {key}"))
}

fn percent_decode(input: &str) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(input.len());
    let source = input.as_bytes();
    let mut index = 0;
    while index < source.len() {
        match source[index] {
            b'+' => {
                bytes.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < source.len() => {
                let hex = std::str::from_utf8(&source[index + 1..index + 3])
                    .map_err(|error| error.to_string())?;
                bytes.push(u8::from_str_radix(hex, 16).map_err(|error| error.to_string())?);
                index += 3;
            }
            byte => {
                bytes.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn event_values(state: &LibraryState) -> Vec<Value> {
    state
        .events
        .iter()
        .map(|event| {
            json!({
                "timestamp_ms": event.timestamp_ms.to_string(),
                "source": event.source,
                "level": event.level,
                "message": event.message
            })
        })
        .collect()
}

fn texture_value(texture: &pbr_material_indexer::Texture) -> Value {
    json!({
        "path": texture.path.to_string_lossy(),
        "filename": texture.path.file_name().unwrap_or_default().to_string_lossy(),
        "kind": texture.kind.label(),
        "dimensions": texture.dimensions.map(|size| json!({"width":size.width,"height":size.height}))
    })
}

fn material_value(index: usize, material: &Material) -> Value {
    json!({
        "index": index,
        "name": material.name,
        "directory": material.directory.to_string_lossy(),
        "anchor": texture_value(&material.anchor),
        "anchor_candidates": material.anchor_candidates.iter().map(texture_value).collect::<Vec<_>>(),
        "textures": material.textures.iter().map(texture_value).collect::<Vec<_>>()
    })
}

fn state_value(app: &AppRuntime) -> Value {
    let Ok(state) = app.state.lock() else {
        return json!({"error":"Application state unavailable"});
    };
    let mut thumbnails_active: Vec<String> = state.thumbnails_active.iter().cloned().collect();
    thumbnails_active.sort();
    json!({
        "root": state.root,
        "scanning": state.scanning,
        "preview_mode": state.preview_mode,
        "thumbnailing": state.thumbnailing,
        "thumbnails_done": state.thumbnails_done,
        "thumbnails_total": state.thumbnails_total,
        "thumbnails_ready": state.thumbnails_ready.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "thumbnails_failed": state.thumbnails_failed.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "thumbnails_active": thumbnails_active,
        "vips_available": app.vips_program.is_some(),
        "status": state.status,
        "files_examined": state.files_examined,
        "entries_seen": state.entries_seen,
        "folders_seen": state.folders_seen,
        "folders_skipped": state.folders_skipped,
        "favorites": state.favorites,
        "materials": state.materials.iter().enumerate().map(|(index, material)| material_value(index, material)).collect::<Vec<_>>(),
        "events": event_values(&state)
    })
}

fn start_scan(app: &Arc<AppRuntime>, root: String) {
    if let Ok(mut state) = app.state.lock() {
        if state.scanning || state.thumbnailing {
            return;
        }
        state.root = root.clone();
        state.materials.clear();
        state.texture_paths.clear();
        state.files_examined = 0;
        state.entries_seen = 0;
        state.folders_seen = 0;
        state.folders_skipped = 0;
        state.scanning = true;
        state.scan_mode_selected = false;
        state.preview_mode = "pure".to_owned();
        state.thumbnailing = false;
        state.thumbnails_done = 0;
        state.thumbnails_total = 0;
        state.thumbnails_ready.clear();
        state.thumbnails_failed.clear();
        state.thumbnails_active.clear();
        state.status = "Scanning map filenames…".to_owned();
    }
    app.record("SCAN", "info", format!("Scan started: {root}"));
    let app = Arc::clone(app);
    thread::spawn(move || {
        let scan_root = PathBuf::from(root);
        // Keep discovery strictly filename-only. Image files are requested later by
        // the browser as visible cards need them, so cloud-backed libraries are not
        // hydrated during indexing.
        let result = scan_with_progress(&scan_root, |progress| record_progress(&app, progress));
        match result {
            Ok(report) => finish_scan(&app, report),
            Err(error) => {
                if let Ok(mut state) = app.state.lock() {
                    state.scanning = false;
                }
                app.record("ERROR", "error", format!("Scan error: {error}"));
            }
        }
    });
}

fn available_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        return ('A'..='Z')
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .filter(|path| path.is_dir())
            .collect();
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

fn folder_listing(path: Option<String>) -> Result<Value, String> {
    let Some(path) = path else {
        let roots: Vec<Value> = available_roots()
            .into_iter()
            .map(|path| json!({"name":path.to_string_lossy(), "path":path.to_string_lossy()}))
            .collect();
        return Ok(json!({"path":null,"parent":null,"folders":roots}));
    };
    let directory = PathBuf::from(path);
    if !directory.is_dir() {
        return Err("The path does not exist or is not a folder.".to_owned());
    }
    let mut folders = Vec::new();
    let entries =
        fs::read_dir(&directory).map_err(|error| format!("Could not read folder: {error}"))?;
    for entry in entries.flatten() {
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            folders.push((name, path));
        }
    }
    folders.sort_by(|left, right| left.0.to_lowercase().cmp(&right.0.to_lowercase()));
    let folders: Vec<Value> = folders
        .into_iter()
        .map(|(name, path)| json!({"name":name,"path":path.to_string_lossy()}))
        .collect();
    let parent = directory
        .parent()
        .map(|path| path.to_string_lossy().into_owned());
    Ok(json!({"path":directory.to_string_lossy(),"parent":parent,"folders":folders}))
}

fn record_progress(app: &AppRuntime, progress: ScanProgress) {
    if let Ok(mut state) = app.state.lock() {
        state.entries_seen = progress.entries_seen;
        state.folders_seen = progress.folders_seen;
    }
    app.record(
        "SCAN",
        "info",
        format!(
            "{} entries · {} folders · {}",
            progress.entries_seen,
            progress.folders_seen,
            progress.current_path.display()
        ),
    );
}

fn finish_scan(app: &Arc<AppRuntime>, report: ScanReport) {
    let material_count = report.materials.len();
    let materials = report.materials;
    let should_build_thumbnails = if let Ok(mut state) = app.state.lock() {
        state.texture_paths = materials
            .iter()
            .flat_map(|material| material.textures.iter().map(|texture| texture.path.clone()))
            .collect();
        state.materials = materials;
        state.files_examined = report.files_examined;
        state.folders_skipped = report.folders_skipped;
        state.scanning = false;
        state.scan_mode_selected && state.preview_mode == "optimized"
    } else {
        false
    };
    app.record(
        "SCAN",
        "info",
        format!(
            "Filename scan complete: {} materials · {} filenames checked · {} folders skipped. Preview images will load on demand in the browser.",
            material_count, report.files_examined, report.folders_skipped
        ),
    );
    if should_build_thumbnails {
        start_thumbnail_generation(app);
    }
}

fn select_scan_mode(app: &Arc<AppRuntime>, mode: &str) -> Result<(), String> {
    if !matches!(mode, "pure" | "optimized") {
        return Err("Choose either pure or optimized library mode.".to_owned());
    }
    if mode == "optimized" && app.vips_program.is_none() {
        return Err("Optimized mode requires libvips. Install libvips and restart Material Library, or choose Pure Library.".to_owned());
    }
    let should_build = {
        let mut state = app
            .state
            .lock()
            .map_err(|_| "Application state unavailable")?;
        state.preview_mode = mode.to_owned();
        state.scan_mode_selected = true;
        !state.scanning && mode == "optimized"
    };
    app.record(
        "PREVIEW",
        "info",
        if mode == "optimized" {
            "Optimized library selected. Original maps remain untouched; 256 px previews will be cached."
        } else {
            "Pure library selected. Original maps will be used for previews."
        },
    );
    if should_build {
        start_thumbnail_generation(app);
    }
    Ok(())
}

fn start_thumbnail_generation(app: &Arc<AppRuntime>) {
    let Some(vips) = app.vips_program.clone() else {
        app.record(
            "ERROR",
            "error",
            "libvips is not installed; optimized previews cannot start.",
        );
        return;
    };
    let paths = {
        let Ok(mut state) = app.state.lock() else {
            return;
        };
        if state.thumbnailing || state.scanning || state.preview_mode != "optimized" {
            return;
        }
        let mut paths: Vec<PathBuf> = state
            .materials
            .iter()
            .flat_map(|material| {
                material
                    .anchor_candidates
                    .iter()
                    .map(|texture| texture.path.clone())
            })
            .collect();
        paths.sort();
        paths.dedup();
        state.thumbnails_total = paths.len();
        state.thumbnails_done = 0;
        state.thumbnails_ready.clear();
        state.thumbnails_failed.clear();
        state.thumbnails_active.clear();
        state.thumbnailing = true;
        paths
    };
    app.record(
        "PREVIEW",
        "info",
        format!(
            "Generating or reusing 256 px previews for {} diffuse/albedo maps.",
            paths.len()
        ),
    );
    let app = Arc::clone(app);
    thread::spawn(move || generate_thumbnails(app, paths, vips));
}

fn generate_thumbnails(app: Arc<AppRuntime>, paths: Vec<PathBuf>, vips: PathBuf) {
    let mut pending = VecDeque::new();
    for path in &paths {
        match thumbnail_cache_path(path) {
            Ok(cache) if cache.is_file() => {
                if let Ok(mut state) = app.state.lock() {
                    state.thumbnails_ready.insert(path.clone());
                    state.thumbnails_done += 1;
                    set_material_dimensions(
                        &mut state.materials,
                        path,
                        read_cached_dimensions(&cache),
                    );
                }
            }
            Ok(_) => pending.push_back(path.clone()),
            Err(error) => {
                app.record(
                    "ERROR",
                    "error",
                    format!(
                        "Could not prepare thumbnail cache for {}: {error}",
                        path.display()
                    ),
                );
                pending.push_back(path.clone());
            }
        }
    }
    if pending.is_empty() {
        if let Ok(mut state) = app.state.lock() {
            state.thumbnailing = false;
        }
        app.record(
            "PREVIEW",
            "info",
            "All optimized previews were already cached.",
        );
        return;
    }

    let pending = Arc::new(Mutex::new(pending));
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4)
        .clamp(1, 8)
        .min(paths.len().max(1));
    let mut handles = Vec::new();
    for _ in 0..workers {
        let app = Arc::clone(&app);
        let pending = Arc::clone(&pending);
        let vips = vips.clone();
        handles.push(thread::spawn(move || loop {
            let next = pending.lock().ok().and_then(|mut queue| queue.pop_front());
            let Some(path) = next else { break };
            let display_path = if let Ok(mut state) = app.state.lock() {
                let root = PathBuf::from(&state.root);
                let display_path = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                state.thumbnails_active.insert(display_path.clone());
                display_path
            } else {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            let result = create_thumbnail(&vips, &path);
            if let Ok(mut state) = app.state.lock() {
                state.thumbnails_active.remove(&display_path);
                state.thumbnails_done += 1;
                match &result {
                    Ok((_, dimensions)) => {
                        state.thumbnails_ready.insert(path.clone());
                        set_material_dimensions(&mut state.materials, &path, *dimensions);
                    }
                    Err(_) => {
                        state.thumbnails_failed.insert(path.clone());
                    }
                }
                let done = state.thumbnails_done;
                let total = state.thumbnails_total;
                drop(state);
                match result {
                    Ok(_) if done == total || done % 10 == 0 => app.record(
                        "PREVIEW",
                        "info",
                        format!("Optimized previews: {done}/{total}"),
                    ),
                    Err(error) => app.record(
                        "ERROR",
                        "error",
                        format!("Thumbnail failed for {}: {error}", path.display()),
                    ),
                    _ => {}
                }
            }
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
    if let Ok(mut state) = app.state.lock() {
        state.thumbnailing = false;
        let ready = state.thumbnails_ready.len();
        let total = state.thumbnails_total;
        let failed = state.thumbnails_failed.len();
        drop(state);
        app.record(
            "PREVIEW",
            "info",
            format!("Optimized preview cache ready: {ready}/{total} created or reused · {failed} unavailable."),
        );
    }
}

fn set_material_dimensions(
    materials: &mut [Material],
    path: &Path,
    dimensions: Option<pbr_material_indexer::ImageDimensions>,
) {
    let Some(dimensions) = dimensions else {
        return;
    };
    for material in materials {
        for texture in material
            .anchor_candidates
            .iter_mut()
            .chain(material.textures.iter_mut())
            .chain(std::iter::once(&mut material.anchor))
            .filter(|texture| texture.path == path)
        {
            texture.dimensions = Some(dimensions);
        }
    }
}

fn create_thumbnail(
    vips: &Path,
    source: &Path,
) -> Result<(PathBuf, Option<pbr_material_indexer::ImageDimensions>), String> {
    let cache = thumbnail_cache_path(source).map_err(|error| error.to_string())?;
    if cache.is_file() {
        return Ok((cache.clone(), read_cached_dimensions(&cache)));
    }
    let parent = cache.parent().ok_or("Invalid thumbnail cache path")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create cache directory: {error}"))?;
    let temporary = cache.with_file_name(format!(
        "{}.{}.tmp.webp",
        cache
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("preview"),
        std::process::id()
    ));
    let output = format!("{}[Q=88]", temporary.to_string_lossy());
    let status = Command::new(vips)
        .arg(source)
        .arg("--size=256x256")
        .arg(format!("--path={output}"))
        .arg("--vips-concurrency=1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("Could not run libvips: {error}"))?;
    if !status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(format!("vipsthumbnail exited with {status}"));
    }
    if !temporary.is_file() {
        return Err("libvips did not create the expected preview file".to_owned());
    }
    let dimensions =
        imagesize::size(source)
            .ok()
            .map(|size| pbr_material_indexer::ImageDimensions {
                width: size.width,
                height: size.height,
            });
    if let Some(size) = dimensions {
        let sidecar = thumbnail_dimensions_path(&cache);
        let _ = fs::write(
            sidecar,
            serde_json::to_vec(&json!({"width":size.width,"height":size.height}))
                .unwrap_or_default(),
        );
    }
    match fs::rename(&temporary, &cache) {
        Ok(()) => Ok((cache, dimensions)),
        Err(_error) if cache.is_file() => {
            let _ = fs::remove_file(temporary);
            let dimensions = read_cached_dimensions(&cache).or(dimensions);
            Ok((cache, dimensions))
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(format!("Could not store thumbnail: {error}"))
        }
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "tga" => "image/x-tga",
        "exr" => "image/x-exr",
        "hdr" => "image/vnd.radiance",
        _ => "application/octet-stream",
    }
}

fn stream_original_image(stream: &mut TcpStream, request: &Request, path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let modified = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let etag = format!("\"{:x}-{:x}\"", metadata.len(), modified);
    let unchanged = request
        .headers
        .iter()
        .find(|(name, _)| name == "if-none-match")
        .is_some_and(|(_, value)| value == &etag);
    if unchanged {
        stream.write_all(format!(
            "HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nCache-Control: private, max-age=3600\r\nConnection: close\r\n\r\n"
        ).as_bytes())?;
        return Ok(());
    }
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: private, max-age=3600\r\nETag: {etag}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        mime_for(path), metadata.len()
    );
    stream.write_all(header.as_bytes())?;
    io::copy(&mut file, stream)?;
    Ok(())
}

fn handle_api(app: &Arc<AppRuntime>, request: &Request, stream: &mut TcpStream) {
    let path = request_path(&request.target);
    match (request.method.as_str(), path) {
        ("GET", "/api/health") => {
            respond_json(stream, 200, json!({"ok":true,"name":"Material Library"}))
        }
        ("GET", "/api/state") => respond_json(stream, 200, state_value(app)),
        ("POST", "/api/favorites") => {
            let result = json_body(request).and_then(|body| {
                let values = body
                    .get("favorites")
                    .and_then(Value::as_array)
                    .ok_or("Favorites must be an array of material keys")?;
                if values.len() > 20_000 {
                    return Err("The favorites list is too large".to_owned());
                }
                let mut favorites = HashSet::with_capacity(values.len());
                for value in values {
                    let key = value
                        .as_str()
                        .ok_or("Every favorite must be a string key")?;
                    if key.len() > 16_384 {
                        return Err("A favorite key is too long".to_owned());
                    }
                    favorites.insert(key.to_owned());
                }
                save_favorites(app, favorites)?;
                Ok(json!({"ok":true}))
            });
            match result {
                Ok(value) => respond_json(stream, 200, value),
                Err(error) => respond_json(stream, 400, json!({"error":error})),
            }
        }
        ("GET", "/api/events") => {
            let events = app
                .state
                .lock()
                .map(|state| event_values(&state))
                .unwrap_or_default();
            respond_json(stream, 200, json!({"events":events}));
        }
        ("GET", "/api/folders") => {
            let path = request.target.split_once('?').and_then(|(_, query)| {
                query.split('&').find_map(|part| {
                    let (key, value) = part.split_once('=')?;
                    if key == "path" {
                        percent_decode(value).ok()
                    } else {
                        None
                    }
                })
            });
            match folder_listing(path) {
                Ok(value) => respond_json(stream, 200, value),
                Err(error) => respond_json(stream, 400, json!({"error":error})),
            }
        }
        ("POST", "/api/set-root") => {
            let result = json_body(request).and_then(|body| {
                let root = body
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("Missing folder path")?;
                let path = PathBuf::from(root);
                if !path.is_dir() {
                    return Err("The selected folder no longer exists.".to_owned());
                }
                let root = path.to_string_lossy().into_owned();
                if let Ok(mut state) = app.state.lock() {
                    state.root = root.clone();
                }
                Ok(json!({"path":root}))
            });
            match result {
                Ok(value) => respond_json(stream, 200, value),
                Err(error) => respond_json(stream, 400, json!({"error":error})),
            }
        }
        ("POST", "/api/scan") => {
            let result = json_body(request).and_then(|body| {
                let root = body
                    .get("root")
                    .and_then(Value::as_str)
                    .ok_or("Missing root path")?
                    .trim();
                if root.is_empty() {
                    return Err("Choose a folder before starting a scan.".to_owned());
                }
                start_scan(app, root.to_owned());
                Ok(json!({"ok":true}))
            });
            match result {
                Ok(value) => respond_json(stream, 200, value),
                Err(error) => respond_json(stream, 400, json!({"error":error})),
            }
        }
        ("POST", "/api/scan-mode") => {
            let result = json_body(request).and_then(|body| {
                let mode = body
                    .get("mode")
                    .and_then(Value::as_str)
                    .ok_or("Missing scan mode")?;
                select_scan_mode(app, mode)?;
                Ok(json!({"ok":true,"mode":mode}))
            });
            match result {
                Ok(value) => respond_json(stream, 200, value),
                Err(error) => respond_json(stream, 400, json!({"error":error})),
            }
        }
        ("GET", "/api/image") => {
            match query_value(&request.target, "path").and_then(|path| app.authorized_path(&path)) {
                Ok(path) => {
                    if let Err(error) = stream_original_image(stream, request, &path) {
                        app.record(
                            "ERROR",
                            "error",
                            format!("Could not stream texture {}: {error}", path.display()),
                        );
                    }
                }
                Err(error) => {
                    app.record("ERROR", "error", format!("Could not read texture: {error}"));
                    respond_json(stream, 400, json!({"error":error}));
                }
            }
        }
        ("GET", "/api/thumbnail") => {
            match query_value(&request.target, "path")
                .and_then(|path| app.authorized_path(&path))
                .and_then(|source| {
                    let cache = thumbnail_cache_path(&source).map_err(|error| error.to_string())?;
                    if cache.is_file() {
                        Ok(cache)
                    } else {
                        Err("Optimized preview is not ready yet".to_owned())
                    }
                }) {
                Ok(path) => {
                    if let Err(error) = stream_original_image(stream, request, &path) {
                        app.record(
                            "ERROR",
                            "error",
                            format!(
                                "Could not stream cached preview {}: {error}",
                                path.display()
                            ),
                        );
                    }
                }
                Err(error) => respond_json(stream, 404, json!({"error":error})),
            }
        }
        ("GET", "/api/file-url") => match query_value(&request.target, "path")
            .and_then(|path| app.authorized_path(&path))
            .and_then(|path| {
                url::Url::from_file_path(path)
                    .map(|url| url.to_string())
                    .map_err(|_| "Could not convert path to a file URL".to_owned())
            }) {
            Ok(url) => respond_json(stream, 200, json!({"url":url})),
            Err(error) => respond_json(stream, 400, json!({"error":error})),
        },
        ("POST", "/api/open-folder") => {
            let result = json_body(request).and_then(|body| {
                let raw = body
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("Missing folder path")?;
                let directory = PathBuf::from(raw);
                let allowed = app
                    .state
                    .lock()
                    .map(|state| {
                        state
                            .materials
                            .iter()
                            .any(|material| material.directory == directory)
                    })
                    .unwrap_or(false);
                if !allowed {
                    return Err("Folder is not part of the current index".to_owned());
                }
                open_folder(&directory).map_err(|error| error.to_string())?;
                app.record(
                    "FILES",
                    "info",
                    format!("Opened folder: {}", directory.display()),
                );
                Ok(json!({"ok":true}))
            });
            match result {
                Ok(value) => respond_json(stream, 200, value),
                Err(error) => {
                    app.record("ERROR", "error", format!("Could not open folder: {error}"));
                    respond_json(stream, 400, json!({"error":error}));
                }
            }
        }
        ("POST", "/api/events/clear") => {
            if let Ok(mut state) = app.state.lock() {
                state.events.clear();
            }
            app.record("UI", "info", "Console cleared");
            respond_json(stream, 200, json!({"ok":true}));
        }
        ("POST", "/api/shutdown") => {
            app.record("UI", "info", "Quit requested");
            respond_json(stream, 200, json!({"ok":true}));
            app.shutdown.store(true, Ordering::SeqCst);
        }
        _ => respond_json(stream, 404, json!({"error":"Endpoint not found"})),
    }
}

fn open_folder(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    command.spawn().map(|_| ())
}

fn handle_connection(app: Arc<AppRuntime>, mut stream: TcpStream, port: u16) {
    let _ = stream.set_nonblocking(false);
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let status = if error.contains("too large") {
                413
            } else {
                400
            };
            respond_json(&mut stream, status, json!({"error":error}));
            return;
        }
    };
    if request.method == "GET" && request_path(&request.target) == "/download-selected" {
        let supplied_token = query_value(&request.target, "token").ok();
        if supplied_token.as_deref() != Some(app.token.as_str()) {
            respond_json(&mut stream, 401, json!({"error":"Invalid local app token"}));
            return;
        }
        let indices = query_value(&request.target, "materials").and_then(|raw| {
            let indices: Result<Vec<usize>, _> = raw.split(',').map(str::parse::<usize>).collect();
            indices.map_err(|error| error.to_string())
        });
        match indices.and_then(|indices| create_material_archive(&app, &indices)) {
            Ok(archive) => {
                app.record(
                    "DOWNLOAD",
                    "info",
                    format!("ZIP ready: {} maps included.", archive.count),
                );
                if let Err(error) = respond_archive(&mut stream, &archive) {
                    app.record(
                        "ERROR",
                        "error",
                        format!("Could not send download: {error}"),
                    );
                }
            }
            Err(error) => {
                app.record("ERROR", "error", format!("Descarga cancelada: {error}"));
                respond_json(&mut stream, 400, json!({"error":error}));
            }
        }
        return;
    }
    if request_path(&request.target).starts_with("/api/") {
        let header_token = request
            .headers
            .iter()
            .find(|(name, _)| name == "x-app-token")
            .map(|(_, value)| value.as_str());
        let query_token = query_value(&request.target, "token").ok();
        if header_token != Some(app.token.as_str())
            && query_token.as_deref() != Some(app.token.as_str())
        {
            respond_json(&mut stream, 401, json!({"error":"Invalid local app token"}));
            return;
        }
        handle_api(&app, &request, &mut stream);
        return;
    }
    match (request.method.as_str(), request_path(&request.target)) {
        ("GET", "/") | ("GET", "/index.html") => {
            let html = include_str!("../web/index.html").replace("__APP_TOKEN__", &app.token);
            respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                html.as_bytes(),
            );
        }
        ("GET", "/style.css") => respond(
            &mut stream,
            200,
            "text/css; charset=utf-8",
            include_str!("../web/style.css").as_bytes(),
        ),
        ("GET", "/app.js") => respond(
            &mut stream,
            200,
            "text/javascript; charset=utf-8",
            include_str!("../web/app.js").as_bytes(),
        ),
        ("GET", "/favicon.svg") => respond(
            &mut stream,
            200,
            "image/svg+xml",
            include_str!("../web/favicon.svg").as_bytes(),
        ),
        _ => {
            let _ = port;
            respond_json(&mut stream, 404, json!({"error":"Not found"}));
        }
    }
}

fn open_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn().map(|_| ())
    }
}

pub fn run() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let token = format!(
        "{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let app = Arc::new(AppRuntime::new(token));
    let url = format!("http://127.0.0.1:{port}/");
    println!("Material Library server ready at {url}");
    if let Err(error) = open_browser(&url) {
        eprintln!("Could not open the browser automatically: {error}");
    }
    while !app.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let app = Arc::clone(&app);
                thread::spawn(move || handle_connection(app, stream, port));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
