param(
    [Parameter(Mandatory = $true)][string]$InputPath,
    [switch]$Force
)

$originalErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = 'Stop'
$workDirectory = $null
$inputLock = $null
try {
    if ($env:OS -ne 'Windows_NT') { throw 'UPX packaging requires native Windows x64.' }
    . (Join-Path $PSScriptRoot 'publish-file.ps1')
    $source = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InputPath)
    if ([IO.Path]::GetExtension($source) -ine '.exe') {
        throw "Input must be a regular, non-symlink executable: $InputPath"
    }
    $inputLock = [SlimeNRF.FilePublisher]::OpenInput($source)
    $source = $inputLock.PathName
    $inputDirectory = [IO.Path]::GetDirectoryName($source)
    $destination = Join-Path $inputDirectory ([IO.Path]::GetFileNameWithoutExtension($source) + '-upx.exe')
    $inputHash = $inputLock.Hash
    $inputBytes = $inputLock.Length
    $workDirectory = [SlimeNRF.FilePublisher]::CreateWorkDirectory($inputDirectory)
    $archive = Join-Path $workDirectory 'upx-5.2.1-win64.zip'
    $archiveHash = 'eabc6792a347d45e945be7748423e7868fd01b0d2bcaa2f4b1031fd71ff69bda'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://github.com/upx/upx/releases/download/v5.2.1/upx-5.2.1-win64.zip' -OutFile $archive
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ine $archiveHash) {
        throw 'UPX 5.2.1 archive SHA256 mismatch; refusing to extract or execute it.'
    }
    $toolDirectory = Join-Path $workDirectory 'tool'
    Expand-Archive -LiteralPath $archive -DestinationPath $toolDirectory
    $upx = Join-Path $toolDirectory 'upx-5.2.1-win64\upx.exe'
    if (-not (Test-Path -LiteralPath $upx -PathType Leaf)) { throw 'Verified UPX archive is missing upx.exe.' }

    function Invoke-PinnedUpx {
        param([string[]]$Arguments)
        $savedPreference = $ErrorActionPreference
        try {
            # As with Cargo, Windows PowerShell's native stderr records are not the exit status.
            $ErrorActionPreference = 'Continue'
            $lines = @(& $upx --no-env @Arguments 2>&1)
            $code = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $savedPreference
        }
        $text = ($lines | ForEach-Object { $_.ToString() }) -join "`n"
        if ($text) { Write-Host $text }
        return [PSCustomObject]@{ ExitCode = $code; Text = $text }
    }

    $snapshot = Join-Path $workDirectory 'input.exe'
    $packed = Join-Path $workDirectory 'packed.exe'
    $restored = Join-Path $workDirectory 'restored.exe'
    $snapshotStream = [IO.File]::Open($snapshot, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $inputLock.Stream.CopyTo($snapshotStream)
    } finally {
        $snapshotStream.Dispose()
    }
    # Highest documented search settings. Retain relocations for ASLR; never force unsafe PE changes.
    $compression = @('--best', '--ultra-brute', '--strip-relocs=0', '--no-progress')
    $exact = $true
    $result = Invoke-PinnedUpx -Arguments ($compression + @('--exact', '-o', $packed, $snapshot))
    if ($result.ExitCode -ne 0 -and
        $result.Text -match "CantPackException: option '--exact' does not work with this file") {
        # UPX 5.2.1 does not implement exact restoration for every format (including win64/pe).
        # This is only an exact-restoration capability fallback, not a compression/security fallback.
        $exact = $false
        Write-Host 'UPX does not support --exact for this executable; retaining maximum compression and testing decompression.'
        if (Test-Path -LiteralPath $packed) { Remove-Item -LiteralPath $packed -Force }
        $result = Invoke-PinnedUpx -Arguments ($compression + @('-o', $packed, $snapshot))
    }
    if ($result.ExitCode -ne 0) { throw "UPX maximum compression failed (exit $($result.ExitCode)); no package was published." }
    $result = Invoke-PinnedUpx -Arguments @('--test', $packed)
    if ($result.ExitCode -ne 0) { throw "UPX integrity test failed (exit $($result.ExitCode)); no package was published." }
    $result = Invoke-PinnedUpx -Arguments @('--decompress', '-o', $restored, $packed)
    if ($result.ExitCode -ne 0) { throw "UPX decompression failed (exit $($result.ExitCode)); no package was published." }
    $restoredHash = (Get-FileHash -LiteralPath $restored -Algorithm SHA256).Hash
    if ($restoredHash -eq $inputHash) {
        Write-Host "Decompression restored byte-identical input (SHA256 $inputHash)."
    } elseif ($exact) {
        throw 'UPX accepted --exact but decompression did not restore the input SHA256; no package was published.'
    } else {
        Write-Host "UPX --exact is unsupported: decompression passed but is not byte-identical (restored SHA256 $restoredHash)."
    }

    $publication = [SlimeNRF.FilePublisher]::Publish($packed, $destination, $Force.IsPresent)
    $outputHash = $publication.Hash
    $outputBytes = $publication.Length
    switch ($publication.State) {
        'Identical' { Write-Host "Identical UPX package already exists: $destination" }
        'Replaced' { Write-Host "Replaced UPX package executable: $destination" }
        'Created' { Write-Host "Packaged separate UPX executable: $destination" }
    }
    Write-Host "Input:  $source ($inputBytes bytes; SHA256 $inputHash)"
    Write-Host "Output: $destination ($outputBytes bytes; SHA256 $outputHash)"
    exit 0
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    if ($inputLock) { $inputLock.Dispose() }
    if ($workDirectory -and (Test-Path -LiteralPath $workDirectory)) {
        Remove-Item -LiteralPath $workDirectory -Recurse -Force
    }
    $ErrorActionPreference = $originalErrorActionPreference
}
