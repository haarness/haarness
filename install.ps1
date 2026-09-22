$ErrorActionPreference = "Stop"
$Repository = if ($env:HAARNESS_REPOSITORY) { $env:HAARNESS_REPOSITORY } else { "haarness/haarness" }
$Version = if ($env:HAARNESS_VERSION) { $env:HAARNESS_VERSION } else { "latest" }
$InstallDir = if ($env:HAARNESS_INSTALL_DIR) { $env:HAARNESS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Haarness\bin" }
$Arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") { "aarch64" } else { "x86_64" }
$Target = "$Arch-pc-windows-msvc"
if ($Version -eq "latest") { $Version = (Invoke-RestMethod "https://api.github.com/repos/$Repository/releases/latest").tag_name }
$Base = "https://github.com/$Repository/releases/download/$Version"
$Temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("haarness-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Temporary | Out-Null
try {
  $Binary = Join-Path $Temporary "haarness.exe"
  Invoke-WebRequest "$Base/haarness-$Target.exe" -OutFile $Binary
  $Checksums = Invoke-WebRequest "$Base/SHA256SUMS"
  $Expected = (($Checksums.Content -split "`n") | Where-Object { $_ -match "haarness-$Target.exe" }) -split "\s+" | Select-Object -First 1
  $Actual = (Get-FileHash -Algorithm SHA256 $Binary).Hash.ToLowerInvariant()
  if ($Actual -ne $Expected.ToLowerInvariant()) { throw "checksum mismatch" }
  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  Move-Item -Force $Binary (Join-Path $InstallDir "haarness.exe")
  Write-Host "Installed $(Join-Path $InstallDir 'haarness.exe')"
} finally { Remove-Item -Recurse -Force $Temporary -ErrorAction SilentlyContinue }

