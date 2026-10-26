# Material Library

A local PBR texture browser and filename-based material indexer, written in Rust. It can also run as a Model Context Protocol (MCP) server for AI agents.

## Features

- Recursively scans a folder and its subfolders by filename and extension. The desktop index pass does not open image files.
- Groups recognized PBR maps by normalized material name and directory, independent of file extension.
- Shows every material in one scrollable list. Choose a pure library that lazily loads original albedo/diffuse images, or an optimized library that creates reusable 256 px WebP previews in the user cache.
- Filters by map type, missing map type, file format, albedo resolution, favorites, and custom filename rules; sorts by name, resolution, or map count.
- Displays the original image through a circular CSS crop. The source file is unchanged.
- Supports filtering, searching, multi-selection, opening a material folder, copying an image URL, and downloading selected texture sets as a ZIP.
- Provides an MCP stdio server with tools for scanning a chosen folder, searching indexed materials, and retrieving a material's maps.
- Runs locally. The web server binds to `127.0.0.1` only.

## Build and run the desktop app

Install the stable Rust toolchain with `rustup`, then run:

```sh
cargo run --release
```

On Windows, `run-material-library.bat` starts the app. The web interface opens in your default browser. For an offline build, first fetch dependencies with `cargo fetch`, then use `cargo run --release --offline`.

The filename scan reads directory entries and filenames only. In pure mode, the browser requests original albedo/diffuse files as their cards enter view. In optimized mode, libvips creates 256 px WebP preview files in the operating system's user cache; source textures are never modified, and cached previews are reused while the source file's path, size, and modified time remain unchanged. Thumbnail generation runs in the background after indexing. It may cause a cloud sync provider to hydrate each original albedo/diffuse file. The original file remains available for opening and downloading. Optimized mode requires libvips. On Windows, install it with `powershell -ExecutionPolicy Bypass -File scripts/install-libvips.ps1`; on macOS or Linux, install it with the system package manager. libvips is licensed under LGPL-2.1-or-later and its official Windows package is downloaded by the installer script. Formats such as EXR, HDR, TIFF, or TGA may not display directly in every browser, but libvips supports a broad range of formats. Favorites are stored in the operating system's per-user application configuration folder.

## MCP server

The same executable supports MCP over stdio. This mode does not start the web UI or open a listening port. A host application starts the process and exchanges newline-delimited JSON-RPC messages over stdin/stdout. Protocol output uses stdout; diagnostics use stderr. It supports the current stateless MCP protocol (`2026-07-28`) through `server/discover`, with a fallback for handshake-era clients through `2025-11-25`.

For safety, MCP scans are limited to folders explicitly allowed when the server starts. Pass one or more `--allow-root <folder>` arguments; a requested scan folder must be inside one of them. Use the narrowest library folder that contains the materials you want agents to search.

Build the executable:

```sh
cargo build --release
```

Add a stdio server entry to your MCP host configuration. Replace the executable path with the location of your build:

```json
{
  "mcpServers": {
    "material-library": {
      "command": "C:\\path\\to\\material-library\\target\\release\\pbr-material-indexer.exe",
      "args": ["--mcp", "--allow-root", "C:\\path\\to\\your\\texture-library"]
    }
  }
}
```

You can also run from a source checkout with `cargo run --release -- --mcp --allow-root "/path/to/your/texture-library"` (set the MCP host's working directory to the cloned repository). Repeat `--allow-root <folder>` to authorize multiple libraries.

### MCP tools

- `scan_material_library(root)`: recursively scans an allowed folder by filenames and keeps the result in the running MCP process. It does not open image data or probe dimensions.
- `search_materials(query?, map_type?, limit?, offset?)`: searches the most recent scan. The default limit is 50 and the maximum is 200.
- `get_material_details(name, directory?)`: returns a material's albedo/diffuse candidates and associated map paths. Pass `directory` when names are ambiguous.

### Browsing and filters

Filter the index by map type, a missing common map, file format, or the albedo/diffuse image's pixel dimensions. Mark materials as favorites; the favorites list is saved in the per-user application configuration directory. Sort by material name, albedo resolution, or number of associated maps.

Advanced filter rows follow Green Search's pattern operators: Contains, Does not contain, Starts with, Ends with, Is, and Is not. Add multiple rules to match all conditions, and use `*` and `?` wildcards. Rules can target the material name, folder, map filename, or map type.

The agent host receives absolute paths from tool results. Only connect this local server to agents you trust, and allow only the folders you intend them to index. The tools currently read/index filenames only; they do not modify source textures or read image contents.

## Recognized map names

Names are split at punctuation and matched on complete tokens. Common resolution and UDIM suffixes (`1k`, `2k`, `4k`, `8k`, `lod0`, and `1001`–`1100`) are ignored before the map token.

| Map | Recognized tokens |
| --- | --- |
| Albedo | `albedo`, `basecolor`, `base_color`, `basecolour`, `color`, `colour`, `col` |
| Diffuse | `diffuse`, `diff`, `diffuse_color` |
| Normal | `normal`, `norm`, `nrm`, `nor`, `normalgl`, `normaldx` |
| Ambient occlusion | `ao`, `occlusion`, `ambientocclusion` |
| Roughness | `roughness`, `rough`, `rgh` |
| Gloss/smoothness | `gloss`, `glossiness`, `smoothness`, `smooth` |
| Metallic | `metallic`, `metalness`, `metal`, `mtl` |
| Specular | `specular`, `spec`, `specularlevel`, `specularcolor` |
| Height | `height`, `hgt` |
| Bump | `bump`, `bumpmap` |
| Displacement | `displacement`, `disp`, `displace` |
| Opacity/alpha | `opacity`, `alpha`, `transparency` |
| Emission | `emission`, `emissive`, `emit` |
| Other | `cavity`, `curvature`, `transmission`, `orm`, `rma`, `mra` |

Supported indexed extensions include PNG, JPG/JPEG, TIFF, TGA, BMP, EXR, WebP, HDR, DDS, and KTX/KTX2. Indexing by extension does not imply that the browser can display every format.

## Association and archive limits

- Maps are associated only when they share a normalized base name and directory. Relationships across folders are not inferred.
- Multiple albedo/diffuse candidates are reported so the user or agent can choose the intended image.
- Selected maps are archived without recompression. The current ZIP writer uses classic ZIP limits: at most 4 GiB and 65,535 entries per archive.
- The desktop index is not persisted between app launches. An MCP scan is retained only for the lifetime of that MCP process.

## Development

```sh
cargo fmt --check
cargo check --all-targets
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidance and [SECURITY.md](SECURITY.md) for local file access details.
