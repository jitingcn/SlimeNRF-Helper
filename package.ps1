param([switch]$Force, [switch]$NoFirmware)

$originalErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = 'Stop'
$originalEnvironment = [Environment]::GetEnvironmentVariables('Process')
$originalDirectory = (Get-Location).Path
try {
    . (Join-Path $PSScriptRoot 'scripts\environment.ps1')
    $upxScript = Join-Path $PSScriptRoot 'scripts\package-upx.ps1'
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
    $destination = Join-Path $dist "slimenrf-helper-$version-windows-x64$variant.exe"
    & $upxScript -InputPath $source -OutputPath $destination -Force:$Force
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    exit 0
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    Restore-SlimeNRFBuildEnvironment -Environment $originalEnvironment -Directory $originalDirectory
    $ErrorActionPreference = $originalErrorActionPreference
}
