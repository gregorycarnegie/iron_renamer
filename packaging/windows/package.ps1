param(
    [string]$OutputPath = "target/msix/iron_renamer-windows-x86_64.msix",
    [ValidateSet("x64", "arm64")][string]$Architecture = "x64"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$exe = Join-Path $root "target/release/iron_renamer.exe"
$stage = Join-Path $root "target/msix/stage"
$assets = Join-Path $stage "Assets"
$output = [IO.Path]::GetFullPath((Join-Path $root $OutputPath))

& cargo build --release --locked
if ($LASTEXITCODE) { exit $LASTEXITCODE }

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item $assets -ItemType Directory -Force | Out-Null
New-Item (Split-Path $output) -ItemType Directory -Force | Out-Null
Copy-Item $exe (Join-Path $stage "iron_renamer.exe")

$cargo = Get-Content (Join-Path $root "Cargo.toml") -Raw
$version = [regex]::Match($cargo, '(?m)^version = "(\d+\.\d+\.\d+)"\r?$').Groups[1].Value
if (!$version) { throw "Could not read the package version from Cargo.toml" }
$manifest = Get-Content (Join-Path $PSScriptRoot "AppxManifest.xml") -Raw
$manifest = $manifest -replace 'Version="0\.0\.0\.0"', "Version=`"$version.0`""
$manifest = $manifest -replace 'ProcessorArchitecture="x64"', "ProcessorArchitecture=`"$Architecture`""
$manifest | Set-Content (Join-Path $stage "AppxManifest.xml") -Encoding utf8

Add-Type -AssemblyName System.Drawing
$source = [Drawing.Image]::FromFile((Join-Path $root "ui/icon.png"))
try {
    foreach ($asset in @{ StoreLogo = 50; Square44x44Logo = 44; Square150x150Logo = 150 }.GetEnumerator()) {
        $bitmap = [Drawing.Bitmap]::new($asset.Value, $asset.Value)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.DrawImage($source, 0, 0, $asset.Value, $asset.Value)
            $bitmap.Save((Join-Path $assets "$($asset.Key).png"), [Drawing.Imaging.ImageFormat]::Png)
        } finally {
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    }
} finally {
    $source.Dispose()
}

$makeAppx = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Filter makeappx.exe -Recurse |
    Where-Object FullName -Match '\\(x64|arm64)\\makeappx\.exe$' |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (!$makeAppx) { throw "MakeAppx.exe not found; install the Windows SDK" }

& $makeAppx pack /d $stage /p $output /o
if ($LASTEXITCODE) { exit $LASTEXITCODE }
Write-Output $output
