param(
    [switch]$NoFirmware,
    [switch]$FormatOnly
)

$ErrorActionPreference = 'Stop'
$originalEnvironment = [Environment]::GetEnvironmentVariables('Process')
$originalDirectory = (Get-Location).Path
try {
    . (Join-Path $PSScriptRoot 'scripts\environment.ps1')
    Initialize-SlimeNRFCargoEnvironment
    Invoke-SlimeNRFCargo -Action fmt -ExtraArguments @('--', '--check')
    if ($script:CargoExitCode -ne 0 -or $FormatOnly) { exit $script:CargoExitCode }

    Initialize-SlimeNRFBuildEnvironment
    Invoke-SlimeNRFCargo -Action clippy -DebugBuild -NoFirmware:$NoFirmware -ExtraArguments @('--all-targets', '--', '-D', 'warnings')
    if ($script:CargoExitCode -ne 0) { exit $script:CargoExitCode }

    Invoke-SlimeNRFCargo -Action test -DebugBuild -NoFirmware:$NoFirmware -ExtraArguments @('--all-targets')
    exit $script:CargoExitCode
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    Restore-SlimeNRFBuildEnvironment -Environment $originalEnvironment -Directory $originalDirectory
}
