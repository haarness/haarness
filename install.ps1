# Install a checksum-verified CLI or Desktop from Haarness Cloud. No administrator rights required.
# Runs in its own scope, so `irm https://haarness.ai/install.ps1 | iex` leaves no variables behind.
& {
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 draws a progress bar per chunk (very slow downloads) and may not offer TLS 1.2.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$Base = if ($env:HAARNESS_RELEASE_URL) { $env:HAARNESS_RELEASE_URL.TrimEnd('/') } else { 'https://haarness.ai' }
$Uri = [Uri]$Base
if ($Uri.Scheme -ne 'https' -and -not ($Uri.Scheme -eq 'http' -and $Uri.IsLoopback)) { throw 'Release URL requires HTTPS' }
$Channel = if ($env:HAARNESS_CHANNEL) { $env:HAARNESS_CHANNEL } else { 'stable' }
$Version = if ($env:HAARNESS_VERSION) { $env:HAARNESS_VERSION } else { 'latest' }
$Product = if ($env:HAARNESS_PRODUCT) { $env:HAARNESS_PRODUCT } else { 'cli' }
if ($Channel -notin @('stable','preview') -or $Version -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._+-]*$' -or $Product -notin @('cli','desktop')) { throw 'Invalid release selection' }
# The machine's own architecture: an x64 PowerShell emulated on ARM64 reports AMD64 everywhere else.
$Native = (Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name PROCESSOR_ARCHITECTURE -ErrorAction SilentlyContinue).PROCESSOR_ARCHITECTURE
if (-not $Native) { $Native = $env:PROCESSOR_ARCHITECTURE }
$Arch = switch ($Native) { 'ARM64' { 'arm64' } 'AMD64' { 'x64' } default { throw "Unsupported architecture: $Native" } }
$InstallDir = if ($env:HAARNESS_INSTALL_DIR) { $env:HAARNESS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Haarness\bin' }
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$InstallDir = (Get-Item -LiteralPath $InstallDir).FullName
$Temporary = Join-Path $InstallDir ('.haarness-install-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Temporary | Out-Null
try {
    $Kind = if ($Product -eq 'cli') { 'binary' } else { 'exe' }
    $Url = "$Base/api/v1/releases/$Channel/$Product/download?os=windows&arch=$Arch&kind=$Kind"
    if ($Version -ne 'latest') { $Url += '&version=' + [Uri]::EscapeDataString($Version) }
    $Binary = Join-Path $Temporary 'haarness.exe'
    try {
        $Response = Invoke-WebRequest -UseBasicParsing $Url -OutFile $Binary -PassThru -MaximumRedirection 0 -TimeoutSec 900
    } catch {
        # Windows PowerShell 5.1 and PowerShell 7 both carry the HTTP status on the exception's response.
        $Status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
        if ($Status -ne 404) { throw }
        if ($Version -eq 'latest') { throw "No $Channel release of Haarness $Product for windows $Arch yet; installation left unchanged" }
        throw "Haarness $Product $Version is not on the $Channel channel for windows $Arch; installation left unchanged"
    }
    $Expected = [string]$Response.Headers['x-content-sha256']
    $Released = [string]$Response.Headers['x-haarness-version']
    if ($Expected -notmatch '^[a-f0-9]{64}$' -or (Get-FileHash -Algorithm SHA256 -LiteralPath $Binary).Hash.ToLowerInvariant() -ne $Expected) { throw 'Checksum mismatch; installation left unchanged' }
    if (-not $Released -or ($Version -ne 'latest' -and $Version -ne $Released)) { throw 'Release version mismatch' }
    # Mark of the Web exists only on Windows ($IsWindows is unset in Windows PowerShell 5.1).
    if ($IsWindows -ne $false) { Unblock-File -LiteralPath $Binary }
    if ($Product -eq 'desktop') {
        $Process = Start-Process -FilePath $Binary -Wait -PassThru
        if ($Process.ExitCode -ne 0) { throw "Desktop installer exited $($Process.ExitCode)" }
        Write-Host "Installed Haarness Desktop $Released. Start it from the Start menu."
    } else {
        try { $Reported = & $Binary --version 2>&1 } catch { $Reported = $_ }
        if ($LASTEXITCODE -ne 0 -or "$Reported" -ne "haarness $Released") {
            throw "The downloaded Haarness does not run on this computer; installation left unchanged. $Reported"
        }
        $Target = Join-Path $InstallDir 'haarness.exe'
        # A running Windows executable cannot be overwritten, but it can be renamed.
        $Backup = Join-Path $InstallDir 'haarness.previous.exe'
        Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $Target) { Move-Item -Force -LiteralPath $Target -Destination $Backup }
        try { Move-Item -LiteralPath $Binary -Destination $Target } catch { if (Test-Path -LiteralPath $Backup) { Move-Item -Force -LiteralPath $Backup -Destination $Target }; throw }
        # Gone unless an older haarness.exe is still running from it.
        Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue
        Write-Host "Installed Haarness $Released at $Target"
        $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $Listed = @(($UserPath, $env:Path) -join ';' -split ';' | ForEach-Object { $_.TrimEnd('\') }) -contains $InstallDir.TrimEnd('\')
        if (-not $Listed) {
            if ($env:HAARNESS_NO_MODIFY_PATH -eq '1') {
                Write-Host "Add $InstallDir to your user PATH to run haarness from any terminal."
            } else {
                [Environment]::SetEnvironmentVariable('Path', ((@($UserPath, $InstallDir) | Where-Object { $_ }) -join ';'), 'User')
                $env:Path = "$env:Path;$InstallDir"
                Write-Host "Added $InstallDir to your user PATH; open a new terminal to run haarness."
            }
        }
    }
} finally { Remove-Item -LiteralPath $Temporary -Recurse -Force -ErrorAction SilentlyContinue }
}
