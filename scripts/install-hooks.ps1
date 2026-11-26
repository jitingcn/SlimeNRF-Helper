$ErrorActionPreference = 'Stop'
try {
    $project = Split-Path $PSScriptRoot -Parent
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
    $contentComparer = [Collections.StructuralComparisons]::StructuralEqualityComparer
    # Preflight both destinations before creating or changing either hook.
    $plans = foreach ($name in 'pre-commit', 'commit-msg') {
        $source = Join-Path $project ".githooks/$name"
        $bytes = [IO.File]::ReadAllBytes($source)
        $destination = Join-Path $hooks $name
        $exists = Test-Path -LiteralPath $destination
        if ($exists -and (
            -not (Test-Path -LiteralPath $destination -PathType Leaf) -or
            -not $contentComparer.Equals($bytes, [IO.File]::ReadAllBytes($destination))
        )) {
            throw "Refusing to replace existing $name hook: $destination. Integrate the project check into that hook manually."
        }
        @{
            Name = $name
            Destination = $destination
            Exists = $exists
            Bytes = $bytes
        }
    }
    [IO.Directory]::CreateDirectory($hooks) | Out-Null
    foreach ($plan in $plans) {
        $name = $plan.Name
        $destination = $plan.Destination
        if (-not $plan.Exists) {
            # CreateNew also refuses a hook created concurrently after preflight.
            $stream = [IO.File]::Open($destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
            try {
                $stream.Write($plan.Bytes, 0, $plan.Bytes.Length)
            } finally {
                $stream.Dispose()
            }
        }
        if ($env:OS -ne 'Windows_NT') {
            & chmod +x $destination
            if ($LASTEXITCODE -ne 0) { throw "Cannot mark hook executable: $destination" }
        }
        if ($plan.Exists) {
            Write-Host "Identical $name hook already exists: $destination"
        } else {
            Write-Host "Installed $name hook: $destination"
        }
    }
    Write-Host 'Existing Git configuration and all other hooks are unchanged.'
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
}
