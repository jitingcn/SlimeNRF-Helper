param(
    [ValidateSet('build', 'run', 'test', 'check', 'clippy', 'fmt', 'clean')]
    [string]$Action = 'build',
    [switch]$Debug,
    [switch]$Release,
    [switch]$NoFirmware,
    [string[]]$CargoArguments = @()
)

$ErrorActionPreference = 'Stop'
$originalEnvironment = [Environment]::GetEnvironmentVariables('Process')
$originalDirectory = (Get-Location).Path
try {
    . (Join-Path $PSScriptRoot 'scripts\environment.ps1')
    if ($Debug -and $Release) { throw 'Specify either -Debug or -Release, not both.' }
    $debugBuild = $Debug -or (-not $Release -and $Action -in @('run', 'test', 'check', 'clippy'))
    if ($Action -in @('clean', 'fmt')) {
        Initialize-SlimeNRFCargoEnvironment
    } else {
        Initialize-SlimeNRFBuildEnvironment
    }
    Invoke-SlimeNRFCargo -Action $Action -DebugBuild:$debugBuild -NoFirmware:$NoFirmware -ExtraArguments $CargoArguments
    exit $script:CargoExitCode
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    Restore-SlimeNRFBuildEnvironment -Environment $originalEnvironment -Directory $originalDirectory
}
