$ErrorActionPreference = 'Stop'
try {
    $project = Split-Path $PSScriptRoot -Parent
    $source = Join-Path $project '.githooks/pre-commit'
    $root = & git -C $project rev-parse --show-toplevel
    if ($LASTEXITCODE -ne 0 -or -not $root) { throw 'Cannot locate the Git working tree.' }
    $projectPath = [IO.Path]::GetFullPath($project).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $rootPath = [IO.Path]::GetFullPath($root).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $comparison = if ($env:OS -eq 'Windows_NT') { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    if (-not [string]::Equals($projectPath, $rootPath, $comparison)) {
        throw "Refusing to install hooks: the application directory must be the Git working tree root. Initialize or clone SlimeNRF Helper as a standalone repository; detected root: $root"
    }
    # Ask Git from its root so relative core.hooksPath values resolve correctly.
    # This also respects linked worktrees and never changes Git configuration.
    $hooks = & git -C $root rev-parse --path-format=absolute --git-path hooks
    if ($LASTEXITCODE -ne 0 -or -not $hooks) { throw 'Cannot resolve the effective Git hooks directory.' }
    $destination = Join-Path $hooks 'pre-commit'
    if (Test-Path -LiteralPath $destination) {
        if ((Test-Path -LiteralPath $destination -PathType Leaf) -and
            (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -eq
            (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash) {
            if ($env:OS -ne 'Windows_NT') {
                & chmod +x -- $destination
                if ($LASTEXITCODE -ne 0) { throw "Cannot mark hook executable: $destination" }
            }
            Write-Host "Identical pre-commit hook already exists: $destination"
            exit 0
        }
        throw "Refusing to replace existing pre-commit hook: $destination. Integrate the format check into that hook manually."
    }
    [IO.Directory]::CreateDirectory($hooks) | Out-Null
    $bytes = [IO.File]::ReadAllBytes($source)
    # CreateNew also refuses a hook created concurrently after the check above.
    $stream = [IO.File]::Open($destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
    try {
        $stream.Write($bytes, 0, $bytes.Length)
    } finally {
        $stream.Dispose()
    }
    if ($env:OS -ne 'Windows_NT') {
        & chmod +x -- $destination
        if ($LASTEXITCODE -ne 0) { throw "Cannot mark hook executable: $destination" }
    }
    Write-Host "Installed pre-commit hook: $destination"
    Write-Host 'Existing Git configuration and all other hooks are unchanged.'
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
}
