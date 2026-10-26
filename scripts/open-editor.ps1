$ErrorActionPreference = 'Stop'
$originalEnvironment = [Environment]::GetEnvironmentVariables('Process')
$originalDirectory = (Get-Location).Path
try {
    . (Join-Path $PSScriptRoot 'environment.ps1')
    if ($env:OS -ne 'Windows_NT') { throw 'This helper supplies the Windows native build environment. Open the app folder normally on other platforms.' }
    # --new-window alone still reuses an existing VS Code process and its old
    # environment. Refuse that case instead of silently losing the CRT setup.
    if (Get-Process -Name Code, 'Code - Insiders' -ErrorAction SilentlyContinue) {
        throw 'Close all VS Code windows/processes, then run this helper from an external PowerShell terminal. Existing editors cannot inherit this build environment.'
    }
    $editor = Get-Command code -CommandType Application -ErrorAction Stop | Select-Object -First 1
    Initialize-SlimeNRFBuildEnvironment
    # Match Invoke-SlimeNRFCargo without a hardcoded machine path or new cache.
    $env:CARGO_BUILD_TARGET = $script:WindowsTarget
    & $editor.Source --new-window --wait $script:ProjectDirectory
    exit $LASTEXITCODE
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    Restore-SlimeNRFBuildEnvironment -Environment $originalEnvironment -Directory $originalDirectory
}
