$ErrorActionPreference = "Stop"

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "This installer is for Windows. On macOS or Linux, install libvips with your package manager."
}

$release = Invoke-RestMethod -Uri "https://api.github.com/repos/libvips/build-win64-mxe/releases/latest"
$asset = $release.assets | Where-Object { $_.name -match '^vips-dev-x64-web-[0-9][0-9A-Za-z.-]*\.zip$' } | Select-Object -First 1
if (-not $asset) {
    throw "Could not find the official libvips x64 web package in the latest release."
}

$installRoot = Join-Path $env:LOCALAPPDATA "MaterialLibrary\tools\libvips"
$downloadPath = Join-Path $env:TEMP $asset.name
New-Item -ItemType Directory -Force -Path $installRoot | Out-Null
Write-Host "Downloading $($asset.name) from the official libvips project..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $downloadPath
Write-Host "Extracting libvips to $installRoot..."
Expand-Archive -LiteralPath $downloadPath -DestinationPath $installRoot -Force

$thumbnailer = Get-ChildItem -LiteralPath $installRoot -Filter "vipsthumbnail.exe" -File -Recurse | Select-Object -First 1
if (-not $thumbnailer) {
    throw "The package was extracted, but vipsthumbnail.exe was not found."
}

$vipsRoot = Split-Path -Parent (Split-Path -Parent $thumbnailer.FullName)
[Environment]::SetEnvironmentVariable("MATERIAL_LIBRARY_VIPS", $vipsRoot, "User")
Write-Host "libvips is installed: $($thumbnailer.FullName)"
Write-Host "Configured MATERIAL_LIBRARY_VIPS for your Windows user. Restart Material Library to enable Optimized Library mode."
