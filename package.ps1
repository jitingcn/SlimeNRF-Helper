param(
    [switch]$Force,
    [switch]$NoFirmware,
    [switch]$Upx,
    [string]$OfficialReleaseTag,
    [string]$FirmwareTag
)

$hasOfficialReleaseTag = $PSBoundParameters.ContainsKey('OfficialReleaseTag')
$hasFirmwareTag = $PSBoundParameters.ContainsKey('FirmwareTag')
$originalErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = 'Stop'
$originalEnvironment = [Environment]::GetEnvironmentVariables('Process')
$originalDirectory = (Get-Location).Path
try {
    . (Join-Path $PSScriptRoot 'scripts\environment.ps1')
    # A release marker is an explicit packaging input, never an inherited default.
    [Environment]::SetEnvironmentVariable('SLIMENRF_OFFICIAL_RELEASE', $null, 'Process')
    [Environment]::SetEnvironmentVariable('SLIMENRF_FIRMWARE_TAG', $null, 'Process')
    if ($hasFirmwareTag) {
        if ($NoFirmware -or $hasOfficialReleaseTag) {
            throw 'FirmwareTag requires a local package with embedded firmware.'
        }
        if ([string]::IsNullOrWhiteSpace($FirmwareTag)) {
            throw 'FirmwareTag must be a nonempty firmware release tag.'
        }
        $env:SLIMENRF_FIRMWARE_TAG = $FirmwareTag
    }
    if ($hasOfficialReleaseTag) {
        if ($Upx) { throw 'Official releases use the uncompressed Windows executable.' }
        if ([string]::IsNullOrWhiteSpace($OfficialReleaseTag)) {
            throw 'OfficialReleaseTag must be a nonempty release tag.'
        }
        $env:SLIMENRF_OFFICIAL_RELEASE = "release:$OfficialReleaseTag"
    }
    Initialize-SlimeNRFBuildEnvironment
    Invoke-SlimeNRFCargo -Action build -NoFirmware:$NoFirmware
    if ($script:CargoExitCode -ne 0) { exit $script:CargoExitCode }

    Push-Location -LiteralPath $PSScriptRoot
    try {
        $metadataText = & cargo metadata --locked --no-deps --format-version 1 --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml')
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $metadata = ($metadataText -join "`n") | ConvertFrom-Json
    } finally {
        Pop-Location
    }
    $package = @($metadata.packages | Where-Object { $_.name -eq 'slimenrf-helper' })
    if ($package.Count -ne 1) { throw 'Cargo metadata must contain exactly one slimenrf-helper package.' }
    $version = $package[0].version
    if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)*$') { throw "Unsafe package version: $version" }
    $source = Join-Path $metadata.target_directory "$script:WindowsTarget\release\slimenrf-helper.exe"
    $dist = Join-Path $PSScriptRoot 'dist'
    [IO.Directory]::CreateDirectory($dist) | Out-Null
    $variant = if ($NoFirmware) { '-no-firmware' } else { '' }
    $filename = "slimenrf-helper-$version-windows-x64$variant"
    if ($Upx) {
        $destination = Join-Path $dist "$filename-upx.exe"
        & (Join-Path $PSScriptRoot 'scripts\package-upx.ps1') -InputPath $source -OutputPath $destination -Force:$Force
        exit $LASTEXITCODE
    }
    . (Join-Path $PSScriptRoot 'scripts\publish-file.ps1')
    $destination = Join-Path $dist "$filename.exe"
    $result = [SlimeNRF.FilePublisher]::Publish($source, $destination, $Force.IsPresent)
    Write-Host "$($result.State): $destination ($($result.Length) bytes; SHA256 $($result.Hash))"
    exit 0
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    Restore-SlimeNRFBuildEnvironment -Environment $originalEnvironment -Directory $originalDirectory
    $ErrorActionPreference = $originalErrorActionPreference
}
