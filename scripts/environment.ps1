$ErrorActionPreference = 'Stop'
$script:ProjectDirectory = Split-Path $PSScriptRoot -Parent
$script:WindowsTarget = 'x86_64-pc-windows-msvc'

function Initialize-SlimeNRFCargoEnvironment {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Cargo is required on PATH.' }
    if ($env:CARGO_TARGET_DIR) {
        $targetDirectory = $env:CARGO_TARGET_DIR
        if (-not [IO.Path]::IsPathRooted($targetDirectory)) {
            $targetDirectory = Join-Path $script:ProjectDirectory $targetDirectory
        }
        $env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($targetDirectory)
    }
}

function Initialize-SlimeNRFBuildEnvironment {
    if ($env:OS -ne 'Windows_NT') { throw 'These scripts package native Windows x64 builds. Use Cargo directly on other platforms.' }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Install Visual Studio with Desktop development with C++ and CMake tools.' }
    $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $vs) { throw 'Visual Studio C++ build tools were not found.' }
    Import-Module (Join-Path $vs 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
    Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64'
    $cmake = Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'
    if (Test-Path -LiteralPath (Join-Path $cmake 'cmake.exe')) { $env:PATH = "$cmake;$env:PATH" }
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) { throw 'CMake is required on PATH.' }
    $llvm = Join-Path $env:ProgramFiles 'LLVM\bin'
    if (Test-Path -LiteralPath (Join-Path $llvm 'libclang.dll')) {
        $env:LIBCLANG_PATH = $llvm
        $env:PATH = "$llvm;$env:PATH"
    }
    Initialize-SlimeNRFCargoEnvironment
    # An explicit target keeps proc macros/build scripts on the normal host CRT.
    if (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) {
        $separator = [char]0x1f
        $extra = "-C${separator}target-feature=+crt-static"
        if ($env:CARGO_ENCODED_RUSTFLAGS) {
            $env:CARGO_ENCODED_RUSTFLAGS += "${separator}${extra}"
        } else {
            $env:CARGO_ENCODED_RUSTFLAGS = $extra
        }
    } else {
        $env:RUSTFLAGS = "$env:RUSTFLAGS -C target-feature=+crt-static".Trim()
    }
}

function Invoke-SlimeNRFCargo {
    param([string]$Action = 'build', [switch]$DebugBuild, [switch]$NoFirmware, [string[]]$ExtraArguments = @())
    $arguments = @($Action, '--manifest-path', (Join-Path $script:ProjectDirectory 'Cargo.toml'))
    # Do not add a competing selection when the caller chooses packages.
    # Arguments after -- belong to the invoked program or test harness.
    $hasPackageSelection = $false
    foreach ($argument in $ExtraArguments) {
        if ($argument -eq '--') { break }
        if ($argument -match '^(?:--(?:package|exclude)(?:=|$)|--(?:workspace|all)$|-p)') {
            $hasPackageSelection = $true
            break
        }
    }
    if (-not $hasPackageSelection) {
        switch ($Action) {
            { $_ -in @('build', 'run') } { $arguments += @('--package', 'slimenrf-helper') }
            { $_ -in @('test', 'check', 'clippy') } { $arguments += '--workspace' }
            'fmt' { $arguments += '--all' }
        }
    }
    if ($Action -ne 'fmt') {
        $arguments += '--locked'
    }
    if ($Action -notin @('fmt', 'clean')) {
        $arguments += @('--target', $script:WindowsTarget)
        if (-not $DebugBuild) { $arguments += '--release' }
        if ($NoFirmware) { $arguments += '--no-default-features' }
    }
    $arguments += $ExtraArguments
    Push-Location -LiteralPath $script:ProjectDirectory
    try {
        $cargo = Get-Command cargo -ErrorAction Stop
        $originalErrorActionPreference = $ErrorActionPreference
        try {
            # Windows PowerShell can surface redirected native stderr as error
            # records. Cargo progress belongs on stderr; its exit code decides.
            $ErrorActionPreference = 'Continue'
            & $cargo @arguments
            $script:CargoExitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $originalErrorActionPreference
        }
    } finally {
        Pop-Location
    }
}

function Set-SlimeNRFEmptyProcessVariable {
    param([string]$Name)
    # .NET Framework treats an empty value as deletion. Win32 distinguishes
    # a non-null empty string from null, preserving explicitly empty entries.
    if (-not ('SlimeNRF.Build.NativeEnvironment' -as [type])) {
        Add-Type -TypeDefinition @'
using System.Runtime.InteropServices;
namespace SlimeNRF.Build {
    public static class NativeEnvironment {
        [DllImport("kernel32.dll", EntryPoint = "SetEnvironmentVariableW",
            CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool SetVariable(string name, string value);
    }
}
'@
    }
    if (-not [SlimeNRF.Build.NativeEnvironment]::SetVariable($Name, '')) {
        throw [System.ComponentModel.Win32Exception]::new(
            [System.Runtime.InteropServices.Marshal]::GetLastWin32Error())
    }
}

function Restore-SlimeNRFBuildEnvironment {
    param([System.Collections.IDictionary]$Environment, [string]$Directory)
    try {
        $current = [Environment]::GetEnvironmentVariables('Process')
        foreach ($name in $current.Keys) {
            if (-not $Environment.Contains($name)) {
                [Environment]::SetEnvironmentVariable($name, $null, 'Process')
            }
        }
        foreach ($name in $Environment.Keys) {
            if (-not $current.Contains($name) -or $current[$name] -cne $Environment[$name]) {
                if ([string]$Environment[$name] -ceq '') {
                    Set-SlimeNRFEmptyProcessVariable -Name $name
                } else {
                    [Environment]::SetEnvironmentVariable($name, $Environment[$name], 'Process')
                }
            }
        }
    } finally {
        Set-Location -LiteralPath $Directory
    }
}
