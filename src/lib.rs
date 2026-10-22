//! Recursive PBR texture discovery and filename-based map association.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MapKind {
    Albedo,
    Diffuse,
    Normal,
    AmbientOcclusion,
    Roughness,
    Glossiness,
    Metallic,
    Specular,
    Height,
    Bump,
    Displacement,
    Opacity,
    Emission,
    Cavity,
    Curvature,
    Transmission,
    PackedOrm,
    PackedRma,
    PackedMra,
}

impl MapKind {
    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "albedo" | "basecolor" | "basecolour" | "color" | "colour" | "col" => Self::Albedo,
            "diffuse" | "diff" => Self::Diffuse,
            "normal" | "norm" | "nrm" | "nor" | "normalgl" | "normaldx" => Self::Normal,
            "ao" | "occlusion" | "ambientocclusion" => Self::AmbientOcclusion,
            "roughness" | "rough" | "rgh" => Self::Roughness,
            "gloss" | "glossiness" | "smoothness" | "smooth" => Self::Glossiness,
            "metallic" | "metalness" | "metal" | "mtl" => Self::Metallic,
            "specular" | "spec" | "specularlevel" | "specularcolor" => Self::Specular,
            "height" | "hgt" => Self::Height,
            "bump" | "bumpmap" => Self::Bump,
            "displacement" | "disp" | "displace" => Self::Displacement,
            "opacity" | "alpha" | "transparency" => Self::Opacity,
            "emission" | "emissive" | "emit" => Self::Emission,
            "cavity" | "cav" => Self::Cavity,
            "curvature" | "curv" => Self::Curvature,
            "transmission" | "translucency" => Self::Transmission,
            "orm" => Self::PackedOrm,
            "rma" => Self::PackedRma,
            "mra" => Self::PackedMra,
            _ => return None,
        })
    }

    pub fn is_anchor(self) -> bool {
        matches!(self, Self::Albedo | Self::Diffuse)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Albedo => "Albedo",
            Self::Diffuse => "Diffuse",
            Self::Normal => "Normal",
            Self::AmbientOcclusion => "AO",
            Self::Roughness => "Roughness",
            Self::Glossiness => "Glossiness / Smoothness",
            Self::Metallic => "Metallic",
            Self::Specular => "Specular",
            Self::Height => "Height",
            Self::Bump => "Bump",
            Self::Displacement => "Displacement",
            Self::Opacity => "Opacity / Alpha",
            Self::Emission => "Emission",
            Self::Cavity => "Cavity",
            Self::Curvature => "Curvature",
            Self::Transmission => "Transmission",
            Self::PackedOrm => "Packed ORM",
            Self::PackedRma => "Packed RMA",
            Self::PackedMra => "Packed MRA",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub path: PathBuf,
    pub kind: MapKind,
}

#[derive(Debug, Clone)]
pub struct Material {
    pub name: String,
    pub directory: PathBuf,
    pub anchor: Texture,
    pub anchor_candidates: Vec<Texture>,
    pub textures: Vec<Texture>,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub materials: Vec<Material>,
    pub files_examined: usize,
    pub folders_skipped: usize,
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub entries_seen: usize,
    pub folders_seen: usize,
    pub current_path: PathBuf,
}

#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    directory: PathBuf,
    base: String,
    display_name: String,
    kind: MapKind,
}

/// Walks a directory tree, grouping recognized maps by normalized base name
/// within each folder. It does not follow directory symlinks.
pub fn scan(root: impl AsRef<Path>) -> io::Result<ScanReport> {
    scan_with_progress(root, |_| {})
}

/// Like `scan`, but periodically reports traversal progress without opening files.
pub fn scan_with_progress(
    root: impl AsRef<Path>,
    mut on_progress: impl FnMut(ScanProgress),
) -> io::Result<ScanReport> {
    let root = root.as_ref();
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Root folder does not exist",
        ));
    }

    let mut report = ScanReport::default();
    let mut files = Vec::new();
    let mut entries_seen = 0usize;
    let mut folders_seen = 0usize;
    let mut last_progress = Instant::now();
    collect_files(
        root,
        &mut files,
        &mut report.folders_skipped,
        &mut entries_seen,
        &mut folders_seen,
        &mut last_progress,
        &mut on_progress,
    )?;
    on_progress(ScanProgress {
        entries_seen,
        folders_seen,
        current_path: root.to_path_buf(),
    });
    files.sort();
    report.files_examined = files.len();

    let infos: Vec<FileInfo> = files.into_iter().filter_map(inspect_file).collect();
    let mut groups: BTreeMap<(PathBuf, String), Vec<FileInfo>> = BTreeMap::new();
    for info in infos.iter().filter(|info| info.kind.is_anchor()) {
        groups
            .entry((info.directory.clone(), info.base.clone()))
            .or_default()
            .push(info.clone());
    }

    for ((directory, base), anchors) in groups {
        let anchor_info = anchors.first().expect("group always has an anchor");
        let anchor_candidates: Vec<Texture> = anchors
            .iter()
            .map(|info| Texture {
                path: info.path.clone(),
                kind: info.kind,
            })
            .collect();
        let anchor = anchor_candidates
            .first()
            .expect("group has an anchor")
            .clone();
        let mut textures: Vec<Texture> = infos
            .iter()
            .filter(|info| info.directory == directory && info.base == base)
            .map(|info| Texture {
                path: info.path.clone(),
                kind: info.kind,
            })
            .collect();
        textures.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.path.cmp(&b.path)));
        report.materials.push(Material {
            name: anchor_info.display_name.clone(),
            directory,
            anchor,
            anchor_candidates,
            textures,
        });
    }
    report.materials.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.directory.cmp(&b.directory))
    });
    Ok(report)
}

fn collect_files(
    directory: &Path,
    files: &mut Vec<PathBuf>,
    skipped: &mut usize,
    entries_seen: &mut usize,
    folders_seen: &mut usize,
    last_progress: &mut Instant,
    on_progress: &mut impl FnMut(ScanProgress),
) -> io::Result<()> {
    *folders_seen += 1;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            *skipped += 1;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    for entry in entries {
        *entries_seen += 1;
        if *entries_seen % 500 == 0 || last_progress.elapsed().as_secs() >= 2 {
            on_progress(ScanProgress {
                entries_seen: *entries_seen,
                folders_seen: *folders_seen,
                current_path: directory.to_path_buf(),
            });
            *last_progress = Instant::now();
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                *skipped += 1;
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                *skipped += 1;
                continue;
            }
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(
                &path,
                files,
                skipped,
                entries_seen,
                folders_seen,
                last_progress,
                on_progress,
            )?;
        } else if file_type.is_file() && supported_extension(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "tif"
                    | "tiff"
                    | "tga"
                    | "bmp"
                    | "exr"
                    | "webp"
                    | "hdr"
                    | "dds"
                    | "ktx"
                    | "ktx2"
            )
        })
        .unwrap_or(false)
}

fn inspect_file(path: PathBuf) -> Option<FileInfo> {
    let stem = path.file_stem()?.to_str()?;
    let tokens: Vec<String> = stem
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let (map_index, kind) = find_map_marker(&tokens)?;
    let before_map = &tokens[..map_index];
    if before_map.is_empty() {
        return None;
    }
    let mut base_tokens: Vec<&str> = before_map.iter().map(String::as_str).collect();
    while base_tokens
        .last()
        .is_some_and(|token| is_variant_token(token))
    {
        base_tokens.pop();
    }
    if base_tokens.is_empty() {
        return None;
    }
    let base = base_tokens.join("_");
    let display_name = base_tokens
        .iter()
        .map(|token| {
            let mut chars = token.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(FileInfo {
        directory: path.parent()?.to_path_buf(),
        path,
        base,
        display_name,
        kind,
    })
}

fn find_map_marker(tokens: &[String]) -> Option<(usize, MapKind)> {
    let mut markers = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let pair = tokens.get(index + 1).map(String::as_str);
        let paired = match (tokens[index].as_str(), pair) {
            ("base", Some("color" | "colour")) => Some(MapKind::Albedo),
            ("ambient", Some("occlusion")) => Some(MapKind::AmbientOcclusion),
            ("specular", Some("level" | "color" | "colour")) => Some(MapKind::Specular),
            ("bump", Some("map")) => Some(MapKind::Bump),
            ("diffuse", Some("color" | "colour")) => Some(MapKind::Diffuse),
            _ => None,
        };
        if let Some(kind) = paired {
            markers.push((index, kind));
            index += 2;
        } else {
            if let Some(kind) = MapKind::from_token(&tokens[index]) {
                markers.push((index, kind));
            }
            index += 1;
        }
    }
    markers.into_iter().rev().find(|(index, _)| *index > 0)
}

fn is_variant_token(token: &str) -> bool {
    matches!(
        token,
        "1k" | "2k" | "4k" | "8k" | "16k" | "low" | "high" | "lod0" | "lod1" | "lod2"
    ) || token
        .parse::<u16>()
        .is_ok_and(|number| (1001..=1100).contains(&number))
}
