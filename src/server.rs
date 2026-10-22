use crc32fast::Hasher as Crc32;
use pbr_material_indexer::{scan_with_progress, Material, ScanProgress, ScanReport};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
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
    files_examined: usize,
    folders_skipped: usize,
    status: String,
    events: VecDeque<Event>,
}

struct AppRuntime {
    state: Mutex<LibraryState>,
    shutdown: AtomicBool,
    token: String,
}

impl AppRuntime {
    fn new(token: String) -> Self {
        Self {
            state: Mutex::new(LibraryState {
                status: "Choose a folder to get started.".to_owned(),
                ..Default::default()
            }),
            shutdown: AtomicBool::new(false),
            token,
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
        "kind": texture.kind.label()
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
    json!({
        "root": state.root,
        "scanning": state.scanning,
        "status": state.status,
        "files_examined": state.files_examined,
        "folders_skipped": state.folders_skipped,
        "materials": state.materials.iter().enumerate().map(|(index, material)| material_value(index, material)).collect::<Vec<_>>(),
        "events": event_values(&state)
    })
}

fn start_scan(app: &Arc<AppRuntime>, root: String) {
    if let Ok(mut state) = app.state.lock() {
        if state.scanning {
            return;
        }
        state.root = root.clone();
        state.materials.clear();
        state.texture_paths.clear();
        state.files_examined = 0;
        state.folders_skipped = 0;
        state.scanning = true;
        state.status = "Scanning map filenames…".to_owned();
    }
    app.record("SCAN", "info", format!("Scan started: {root}"));
    let app = Arc::clone(app);
    thread::spawn(move || {
        let scan_root = PathBuf::from(root);
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

fn finish_scan(app: &AppRuntime, report: ScanReport) {
    let material_count = report.materials.len();
    let materials = report.materials;
    if let Ok(mut state) = app.state.lock() {
        state.texture_paths = materials
            .iter()
            .flat_map(|material| material.textures.iter().map(|texture| texture.path.clone()))
            .collect();
        state.materials = materials;
        state.files_examined = report.files_examined;
        state.folders_skipped = report.folders_skipped;
        state.scanning = false;
    }
    app.record(
        "SCAN",
        "info",
        format!(
            "Dry run complete: {} materials · {} filenames checked · {} folders skipped. No image data was opened.",
            material_count, report.files_examined, report.folders_skipped
        ),
    );
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
