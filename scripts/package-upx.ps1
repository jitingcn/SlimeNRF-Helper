param(
    [Parameter(Mandatory = $true)][string]$InputPath,
    [Parameter(Mandatory = $true)][string]$OutputPath,
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
    $destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
    $outputDirectory = [IO.Path]::GetDirectoryName($destination)
    $inputHash = $inputLock.Hash
    $inputBytes = $inputLock.Length
    $workDirectory = [SlimeNRF.FilePublisher]::CreateWorkDirectory($outputDirectory)
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
    # Use LZMA without exhaustive search. Retain relocations for ASLR; never force unsafe PE changes.
    $compression = @('--best', '--lzma', '--strip-relocs=0', '--no-progress')
    $result = Invoke-PinnedUpx -Arguments ($compression + @('-o', $packed, $snapshot))
    if ($result.ExitCode -ne 0) { throw "UPX compression failed (exit $($result.ExitCode)); no package was published." }
    $result = Invoke-PinnedUpx -Arguments @('--test', $packed)
    if ($result.ExitCode -ne 0) { throw "UPX integrity test failed (exit $($result.ExitCode)); no package was published." }
    $result = Invoke-PinnedUpx -Arguments @('--decompress', '-o', $restored, $packed)
    if ($result.ExitCode -ne 0) { throw "UPX decompression failed (exit $($result.ExitCode)); no package was published." }
    $restoredHash = (Get-FileHash -LiteralPath $restored -Algorithm SHA256).Hash
    if ($restoredHash -eq $inputHash) {
        Write-Host "Decompression restored byte-identical input (SHA256 $inputHash)."
    } else {
        Write-Host "Decompression passed; the restored executable is not byte-identical (SHA256 $restoredHash)."
    }

    $publication = [SlimeNRF.FilePublisher]::Publish($packed, $destination, $Force.IsPresent)
    $outputHash = $publication.Hash
    $outputBytes = $publication.Length
    switch ($publication.State) {
        'Identical' { Write-Host "Identical UPX package already exists: $destination" }
        'Replaced' { Write-Host "Replaced UPX package executable: $destination" }
        'Created' { Write-Host "Packaged UPX executable: $destination" }
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
