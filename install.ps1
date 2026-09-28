$ErrorActionPreference = 'Stop'
$Base = if ($env:HAARNESS_RELEASE_URL) { $env:HAARNESS_RELEASE_URL.TrimEnd('/') } else { 'https://app.haarness.dev' }
$Uri = [Uri]$Base
if ($Uri.Scheme -ne 'https' -and -not ($Uri.Scheme -eq 'http' -and $Uri.IsLoopback)) { throw 'Release URL requires HTTPS' }
$Channel = if ($env:HAARNESS_CHANNEL) { $env:HAARNESS_CHANNEL } else { 'stable' }
$Version = if ($env:HAARNESS_VERSION) { $env:HAARNESS_VERSION } else { 'latest' }
$Product = if ($env:HAARNESS_PRODUCT) { $env:HAARNESS_PRODUCT } else { 'cli' }
if ($Channel -notin @('stable','preview') -or $Version -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._+-]*$' -or $Product -notin @('cli','desktop')) { throw 'Invalid release selection' }
$Arch = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) { 'Arm64' { 'arm64' }; 'X64' { 'x64' }; default { throw 'Unsupported architecture' } }
$InstallDir = if ($env:HAARNESS_INSTALL_DIR) { $env:HAARNESS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Haarness\bin' }
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$Temporary = Join-Path $InstallDir ('.haarness-install-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Temporary | Out-Null
try {
    $Kind = if ($Product -eq 'cli') { 'binary' } else { 'exe' }
    $Url = "$Base/api/v1/releases/$Channel/$Product/download?os=windows&arch=$Arch&kind=$Kind"
    if ($Version -ne 'latest') { $Url += '&version=' + [Uri]::EscapeDataString($Version) }
    $Binary = Join-Path $Temporary 'haarness.exe'
    $Response = Invoke-WebRequest -UseBasicParsing $Url -OutFile $Binary -PassThru -MaximumRedirection 0
    $Expected = [string]$Response.Headers['x-content-sha256']
    $Released = [string]$Response.Headers['x-haarness-version']
    if ($Expected -notmatch '^[a-f0-9]{64}$' -or (Get-FileHash -Algorithm SHA256 $Binary).Hash.ToLowerInvariant() -ne $Expected) { throw 'Checksum mismatch; installation left unchanged' }
    if (-not $Released -or ($Version -ne 'latest' -and $Version -ne $Released)) { throw 'Release version mismatch' }
    Unblock-File -LiteralPath $Binary
    if ($Product -eq 'desktop') {
        $Process = Start-Process -FilePath $Binary -Wait -PassThru
        if ($Process.ExitCode -ne 0) { throw "Desktop installer exited $($Process.ExitCode)" }
    } else {
        if ((& $Binary --version) -ne "haarness $Released" -or $LASTEXITCODE -ne 0) { throw 'Binary version mismatch' }
        $Target = Join-Path $InstallDir 'haarness.exe'
        # A running Windows executable cannot be overwritten. Rename it first.
        $Backup = Join-Path $InstallDir 'haarness.previous.exe'
        if (Test-Path $Target) { Move-Item -Force $Target $Backup }
        try { Move-Item $Binary $Target } catch { if (Test-Path $Backup) { Move-Item -Force $Backup $Target }; throw }
        Write-Host "Installed Haarness $Released at $Target"
        if (($env:Path -split ';') -notcontains $InstallDir) { Write-Host "Add $InstallDir to your user PATH." }
    }
} finally { Remove-Item -Recurse -Force $Temporary -ErrorAction SilentlyContinue }
