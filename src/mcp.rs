//! Minimal MCP stdio server for local PBR material discovery.

use pbr_material_indexer::{scan, Material, ScanReport};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const LATEST_LEGACY_VERSION: &str = "2025-11-25";
const CURRENT_PROTOCOL_VERSION: &str = "2026-07-28";
static LAST_SCAN: Mutex<Option<ScanReport>> = Mutex::new(None);

pub fn run(arguments: &[String]) -> io::Result<()> {
    let allowed_roots = match parse_allowed_roots(arguments) {
        Ok(roots) => roots,
        Err(error) => {
            eprintln!("MCP configuration error: {error}");
            return Err(io::Error::new(io::ErrorKind::InvalidInput, error));
        }
    };
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_message(message, &allowed_roots),
            Err(error) => Some(json!({
                "jsonrpc":"2.0",
                "id":null,
                "error":{"code":-32700,"message":format!("Parse error: {error}")}
            })),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut stdout, &response)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn handle_message(message: Value, allowed_roots: &[PathBuf]) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    if method == "server/discover" {
        return Some(json!({"jsonrpc":"2.0","id":id,"result":discover_result()}));
    }
    if method == "initialize" {
        let result = initialize(&params);
        return Some(match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}}),
        });
    }
    let modern_request = params
        .pointer("/_meta/io.modelcontextprotocol~1protocolVersion")
        .and_then(Value::as_str);
    if let Some(version) = modern_request {
        if version != CURRENT_PROTOCOL_VERSION {
            return Some(json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{"code":-32022,"message":"Unsupported protocol version","data":{"supportedVersions":[CURRENT_PROTOCOL_VERSION]}}
            }));
        }
    }
    let modern = modern_request == Some(CURRENT_PROTOCOL_VERSION);
    let result: Result<Value, String> = match method {
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list()),
        "tools/call" => {
            let result = call_tool(&params, allowed_roots).unwrap_or_else(
                |error| json!({"content":[{"type":"text","text":error}],"isError":true}),
            );
            return Some(json!({"jsonrpc":"2.0","id":id,"result":protocol_result(result, modern)}));
        }
        _ => {
            return Some(json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{"code":-32601,"message":format!("Method not found: {method}")}
            }))
        }
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":protocol_result(result, modern)}),
        Err(error) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}}),
    })
}

fn initialize(params: &Value) -> Result<Value, String> {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(LATEST_LEGACY_VERSION);
    let legacy_versions = [
        "2024-11-05",
        "2025-03-26",
        "2025-06-18",
        LATEST_LEGACY_VERSION,
    ];
    let negotiated = if legacy_versions.contains(&requested) {
        requested
    } else {
        LATEST_LEGACY_VERSION
    };
    Ok(json!({
        "protocolVersion":negotiated,
        "capabilities":{"tools":{"listChanged":false}},
        "serverInfo":{"name":"material-library","version":env!("CARGO_PKG_VERSION")}
    }))
}

fn discover_result() -> Value {
    json!({
        "resultType":"complete",
        "supportedVersions":[CURRENT_PROTOCOL_VERSION],
        "capabilities":{"tools":{}},
        "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"material-library","version":env!("CARGO_PKG_VERSION")}},
        "instructions":"Scan a user-selected local folder by filenames, then search the indexed PBR materials. The scan does not open image data. Tool results include absolute local paths.",
        "ttlMs":0,
        "cacheScope":"private"
    })
}

fn protocol_result(mut result: Value, modern: bool) -> Value {
    if modern {
        if let Some(object) = result.as_object_mut() {
            object.insert("resultType".to_owned(), json!("complete"));
            object.insert("_meta".to_owned(), json!({
                "io.modelcontextprotocol/serverInfo":{"name":"material-library","version":env!("CARGO_PKG_VERSION")}
            }));
        }
    }
    result
}

fn tool_list() -> Value {
    json!({"tools":[
        {
            "name":"scan_material_library",
            "title":"Scan a material library",
            "description":"Recursively scan a user-selected local folder for PBR textures. Reads filenames and directory entries only; it does not open image data. The scan is stored in this MCP process for later searches.",
            "inputSchema":{"type":"object","properties":{"root":{"type":"string","description":"Absolute path to the folder to scan"}},"required":["root"]},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true}
        },
        {
            "name":"search_materials",
            "title":"Search indexed PBR materials",
            "description":"Search the most recent scan by material name, folder, texture filename, or optional map type. Returns material paths and associated PBR maps. Run scan_material_library first.",
            "inputSchema":{"type":"object","properties":{"query":{"type":"string","description":"Text to match; omit to list all materials"},"map_type":{"type":"string","description":"Optional map type such as Normal, Roughness, Albedo, or Metallic"},"limit":{"type":"integer","minimum":1,"maximum":200,"description":"Maximum number of results (default 50)"},"offset":{"type":"integer","minimum":0,"description":"Result offset (default 0)"}}},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}
        },
        {
            "name":"get_material_details",
            "title":"Get material details",
            "description":"Return the maps and paths for one material by exact name. If names are ambiguous, provide its exact directory path too.",
            "inputSchema":{"type":"object","properties":{"name":{"type":"string","description":"Exact material name"},"directory":{"type":"string","description":"Optional exact directory path to disambiguate results"}},"required":["name"]},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}
        }
    ]})
}

fn call_tool(params: &Value, allowed_roots: &[PathBuf]) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("Missing tool name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let response = match name {
        "scan_material_library" => scan_library(&arguments, allowed_roots)?,
        "search_materials" => search_library(&arguments)?,
        "get_material_details" => material_details(&arguments)?,
        _ => return Err(format!("Unknown tool: {name}")),
    };
    Ok(
        json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&response).unwrap_or_default()}]}),
    )
}

fn scan_library(arguments: &Value, allowed_roots: &[PathBuf]) -> Result<Value, String> {
    let root = arguments
        .get("root")
        .and_then(Value::as_str)
        .ok_or("The root argument is required")?;
    if allowed_roots.is_empty() {
        return Err("No scan roots are allowed. Start the MCP server with one or more --allow-root <folder> arguments.".to_owned());
    }
    let root_path = std::fs::canonicalize(root)
        .map_err(|error| format!("Could not resolve the requested scan folder: {error}"))?;
    if !allowed_roots
        .iter()
        .any(|allowed| root_path.starts_with(allowed))
    {
        return Err(
            "The requested folder is outside the MCP server's configured --allow-root folders."
                .to_owned(),
        );
    }
    if !root_path.is_dir() {
        return Err("The requested scan path is not a folder.".to_owned());
    }
    let report = scan(&root_path)
        .map_err(|error| format!("Could not scan the requested folder: {error}"))?;
    let result = json!({
        "root":root_path.to_string_lossy(),
        "materials_found":report.materials.len(),
        "files_examined":report.files_examined,
        "folders_skipped":report.folders_skipped,
        "image_data_opened":false
    });
    *LAST_SCAN
        .lock()
        .map_err(|_| "MCP scan state is unavailable")? = Some(report);
    Ok(result)
}

fn parse_allowed_roots(arguments: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] != "--allow-root" {
            return Err(format!("Unknown MCP option: {}", arguments[index]));
        }
        index += 1;
        let raw = arguments
            .get(index)
            .ok_or("--allow-root requires a folder path")?;
        let root = std::fs::canonicalize(Path::new(raw))
            .map_err(|error| format!("Could not resolve allowed root '{raw}': {error}"))?;
        if !root.is_dir() {
            return Err(format!("Allowed root is not a folder: {}", root.display()));
        }
        roots.push(root);
        index += 1;
    }
    Ok(roots)
}

fn search_library(arguments: &Value) -> Result<Value, String> {
    let scans = LAST_SCAN
        .lock()
        .map_err(|_| "MCP scan state is unavailable")?;
    let report = scans
        .as_ref()
        .ok_or("Run scan_material_library before searching")?;
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let map_type = arguments
        .get("map_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let matches: Vec<&Material> = report
        .materials
        .iter()
        .filter(|material| {
            let name_matches = query.is_empty()
                || material.name.to_lowercase().contains(&query)
                || material
                    .directory
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query)
                || material.textures.iter().any(|texture| {
                    texture
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query)
                });
            let kind_matches = map_type.is_empty()
                || material
                    .textures
                    .iter()
                    .any(|texture| texture.kind.label().to_lowercase().contains(&map_type));
            name_matches && kind_matches
        })
        .collect();
    let total = matches.len();
    let materials: Vec<Value> = matches
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(material_json)
        .collect();
    Ok(json!({"total_matches":total,"offset":offset,"limit":limit,"materials":materials}))
}

fn material_details(arguments: &Value) -> Result<Value, String> {
    let name = arguments
        .get("name")
        .and_then(Value::as_str)
        .ok_or("The name argument is required")?
        .to_lowercase();
    let directory = arguments.get("directory").and_then(Value::as_str);
    let scans = LAST_SCAN
        .lock()
        .map_err(|_| "MCP scan state is unavailable")?;
    let report = scans
        .as_ref()
        .ok_or("Run scan_material_library before requesting material details")?;
    let matches: Vec<&Material> = report
        .materials
        .iter()
        .filter(|material| {
            material.name.to_lowercase() == name
                && directory.map_or(true, |path| material.directory.to_string_lossy() == path)
        })
        .collect();
    if matches.is_empty() {
        return Err(format!(
            "No material named '{name}' was found in the latest scan"
        ));
    }
    if matches.len() > 1 {
        return Err(format!(
            "More than one material is named '{name}'; pass its exact directory argument"
        ));
    }
    Ok(material_json(matches[0]))
}

fn material_json(material: &Material) -> Value {
    let maps: Vec<Value> = material
        .textures
        .iter()
        .map(|texture| {
            json!({
                "type":texture.kind.label(),
                "path":texture.path.to_string_lossy()
            })
        })
        .collect();
    let anchors: Vec<Value> = material
        .anchor_candidates
        .iter()
        .map(|texture| {
            json!({
                "type":texture.kind.label(),
                "path":texture.path.to_string_lossy()
            })
        })
        .collect();
    json!({
        "name":material.name,
        "directory":material.directory.to_string_lossy(),
        "primary_albedo_or_diffuse":{"type":material.anchor.kind.label(),"path":material.anchor.path.to_string_lossy()},
        "anchor_candidates":anchors,
        "maps":maps
    })
}
