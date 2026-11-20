<#
.SYNOPSIS
  Packages the desktop app for Windows x64, or Windows on ARM.
.DESCRIPTION
  Writes, into $env:DIST (default: dist\), for $env:ARCH (x64, the default, or arm64):
    sumi-<version>-windows-<arch>.msi            the installer (WiX v5), for every user
    sumi-<version>-windows-<arch>-portable.zip   Sumi.exe and the licences, to run anywhere
  An arm64 build can be made on an x64 PC with Visual Studio's ARM64 build tools.
  The C runtime is linked in (+crt-static), so nothing else needs installing. The version is
  $env:VERSION, else the workspace Cargo.toml's [workspace.package] version (the one a release is
  tagged with). Needs Rust (MSVC) and WiX v5 with its UI and Util extensions ($env:WIX_EXE can name another
  wix.exe):
    dotnet tool install --global wix --version 5.0.2
    wix extension add -g WixToolset.UI.wixext/5.0.2 WixToolset.Util.wixext/5.0.2
.EXAMPLE
  pwsh packaging/windows/package.ps1
  pwsh packaging/windows/package.ps1 -SkipBuild
  $env:ARCH = 'arm64'; pwsh packaging/windows/package.ps1
#>
param([switch] $SkipBuild)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

function Invoke-Native([string] $What, [scriptblock] $Block) {
  Write-Output "==> $What"
  & $Block
  if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

$Version = $env:VERSION
if (-not $Version) {
  # walk the [workspace.package] section of Cargo.toml and take its version = "..." line; the
  # capture group drops the quotes, so no separate Trim of them is needed
  $inWorkspacePackage = $false
  foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
    if ($line -match '^\[workspace\.package\]') { $inWorkspacePackage = $true; continue }
    if ($line -match '^\[') { $inWorkspacePackage = $false; continue }
    if ($inWorkspacePackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
      $Version = $Matches[1]
      break
    }
  }
}
if (-not $Version) { throw 'no version: set VERSION or [workspace.package] version in Cargo.toml' }
# the MSI's own version is numbers only: a beta's suffix is left out
$MsiVersion = ($Version -split '-')[0]
# the machine it is for: its Rust target and the PE header's machine number
$Arch = if ($env:ARCH) { $env:ARCH } else { 'x64' }
if ($Arch -eq 'x64') {
  $Target = 'x86_64-pc-windows-msvc'
  $Machine = 0x8664
} elseif ($Arch -eq 'arm64') {
  $Target = 'aarch64-pc-windows-msvc'
  $Machine = 0xAA64
} else {
  throw "ARCH must be x64 or arm64, not $Arch"
}
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist' }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
# WIX_EXE, not WIX: Windows runners set WIX to an old WiX 3's folder
$Wix = if ($env:WIX_EXE) { $env:WIX_EXE } else { 'wix' }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
Write-Output "Sumi $Version for Windows $Arch"

if (-not $SkipBuild) {
  # the C runtime linked in; only for this target, so build scripts and proc-macros are as ever
  Set-Item -Path ('env:CARGO_TARGET_' + ($Target.ToUpper() -replace '-', '_') + '_RUSTFLAGS') -Value '-C target-feature=+crt-static'
  Invoke-Native 'cargo build' { cargo build --release --locked -p sumi-desktop --target $Target }
}
$Exe = Join-Path $TargetDir "$Target\release\sumi.exe"

# the .exe: for this machine, a windowed program (no console), and needing no Visual C++ runtime
$bytes = [System.IO.File]::ReadAllBytes($Exe)
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
$machine = [BitConverter]::ToUInt16($bytes, $pe + 4)
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C)
if ($machine -ne $Machine) { throw ('sumi.exe is for machine 0x{0:X}, not {1}' -f $machine, $Arch) }
if ($subsystem -ne 2) { throw "sumi.exe has PE subsystem $subsystem, not 2 (a window)" }
if ([System.Text.Encoding]::ASCII.GetString($bytes).Contains('VCRUNTIME140.dll')) {
  throw 'sumi.exe needs VCRUNTIME140.dll: the C runtime should be linked in (+crt-static)'
}
Write-Output "ok sumi.exe: $Arch, a window, no Visual C++ runtime needed"

$Stage = Join-Path $TargetDir 'windows-package\stage'
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item $Exe (Join-Path $Stage 'Sumi.exe')
foreach ($f in 'LICENSE-GPL-3.0', 'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md') { Copy-Item (Join-Path $Root $f) $Stage }

# ---- MSI ----------------------------------------------------------------------------------------
$Msi = Join-Path $Dist "sumi-$Version-windows-$Arch.msi"
Invoke-Native 'wix build' {
  & $Wix build (Join-Path $PSScriptRoot 'sumi.wxs') -arch $Arch `
    -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext -culture en-US `
    -d "Version=$MsiVersion" -d "DisplayVersion=$Version" -d "BinDir=$Stage" `
    -d "IconPath=$(Join-Path $Root 'assets\app-icon\sumi.ico')" -o $Msi
}
Invoke-Native 'wix msi validate' {
  & $Wix msi validate $Msi -intermediateFolder (Join-Path $TargetDir 'windows-package\msi-validation')
}
# its debug symbols (.wixpdb) aren't for the release
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))

# ---- portable zip -------------------------------------------------------------------------------
$Name = "sumi-$Version-windows-$Arch-portable"
$Portable = Join-Path $TargetDir "windows-package\$Name"
Remove-Item -Recurse -Force $Portable -ErrorAction SilentlyContinue
Copy-Item -Recurse $Stage $Portable
$Zip = Join-Path $Dist "$Name.zip"
Remove-Item -Force $Zip -ErrorAction SilentlyContinue
Compress-Archive -Path $Portable -DestinationPath $Zip

Get-Item $Msi, $Zip | Format-Table Name, Length
