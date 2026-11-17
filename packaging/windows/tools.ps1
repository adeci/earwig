# Installs what a Windows CI job needs, each tool as tools.psd1 pins it: downloaded from its
# vendor and checked against its SHA-256 there before anything runs it. Later steps of the job
# get it through GITHUB_ENV and GITHUB_PATH, the versions as step outputs (the cargo cache key).
#   pwsh packaging/windows/tools.ps1 Rust VulkanSdk CMake   the build job
#   pwsh packaging/windows/tools.ps1 InnoSetup               the package job
#   pwsh packaging/windows/tools.ps1 Sde                     the test job
[CmdletBinding()]
param(
    [Parameter(Mandatory, ValueFromRemainingArguments)]
    [ValidateSet('Rust', 'VulkanSdk', 'CMake', 'InnoSetup', 'Sde')]
    [string[]]$Tools
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'tools.ps1 sets up a GitHub Actions runner' }
$pins = Import-PowerShellDataFile "$PSScriptRoot\tools.psd1"

# The pinned file, downloaded; one that doesn't match its SHA-256 is deleted unrun.
function Fetch([string]$name) {
    $pin = $pins[$name]
    $dir = Join-Path $env:RUNNER_TEMP 'downloads'
    New-Item -ItemType Directory -Force $dir | Out-Null
    $file = Join-Path $dir ([uri]$pin.Url).Segments[-1]
    Invoke-WebRequest -UseBasicParsing -MaximumRetryCount 3 -RetryIntervalSec 10 -OutFile $file $pin.Url
    $sha256 = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant()
    if ($sha256 -ne $pin.Sha256) {
        Remove-Item $file
        throw "$($pin.Url) has SHA-256 $sha256, not $($pin.Sha256) as packaging/windows/tools.psd1 pins: deleted, nothing of it ran"
    }
    Write-Host "$name $($pin.Version): $($pin.Url), SHA-256 $sha256 as pinned"
    $file
}

function Export([string]$name, [string]$value) {
    Set-Item "Env:$name" $value
    "$name=$value" | Add-Content $env:GITHUB_ENV
}

function AddPath([string]$dir) {
    $env:PATH = "$dir;$env:PATH"
    $dir | Add-Content $env:GITHUB_PATH
}

function Output([string]$name, [string]$value) { "$name=$value" | Add-Content $env:GITHUB_OUTPUT }

# A program to its end within $minutes, which must exit 0.
function Install([string]$exe, [string[]]$arguments, [int]$minutes) {
    $p = Start-Process $exe -ArgumentList $arguments -NoNewWindow -PassThru
    $null = $p.Handle
    if (-not $p.WaitForExit($minutes * 60000)) {
        $p.Kill($true)
        throw "$(Split-Path -Leaf $exe) did not finish within $minutes minutes"
    }
    if ($p.ExitCode) { throw "$(Split-Path -Leaf $exe) exited with $($p.ExitCode)" }
}

# rustup is the runner image's. The toolchain applies to every later cargo through RUSTUP_TOOLCHAIN.
function Rust {
    $pin = $pins.Rust
    $hashes = '(?m)^\w*hash = "([0-9a-f]{64})"'
    $manifest = Get-Content -Raw (Fetch Rust)
    $pinned = [Collections.Generic.HashSet[string]]::new([string[]]@([regex]::Matches($manifest, $hashes) | ForEach-Object { $_.Groups[1].Value }))
    rustup toolchain install $pin.Version --profile minimal --component clippy --no-self-update
    if ($LASTEXITCODE) { throw "rustup toolchain install $($pin.Version) exited with $LASTEXITCODE" }
    # rustup checked each component against the channel manifest it fetched itself and keeps
    # in the toolchain, re-serialized: all of that one's hashes must be the pinned manifest's.
    $sysroot = rustc "+$($pin.Version)" --print sysroot
    $kept = @([regex]::Matches((Get-Content -Raw "$sysroot\lib\rustlib\multirust-channel-manifest.toml"), $hashes) | ForEach-Object { $_.Groups[1].Value })
    $foreign = @($kept | Where-Object { -not $pinned.Contains($_) })
    if (-not $kept -or $foreign) { throw "Rust $($pin.Version) came with another channel manifest than the pinned one ($($foreign.Count) of $($kept.Count) hashes unknown)" }
    Export RUSTUP_TOOLCHAIN $pin.Version
    Output rust $pin.Version
    rustc -Vv
    cargo clippy -V
}

function VulkanSdk {
    $pin = $pins.VulkanSdk
    $sdk = "C:\VulkanSDK\$($pin.Version)"
    Install (Fetch VulkanSdk) '--root', $sdk, '--accept-licenses', '--default-answer', '--confirm-command', 'install' 10
    # The SDK's runtime installer put LunarG's loader in System32 over Windows' own; the
    # packages ship it from the SDK's runtime folder, where package.ps1 looks first.
    $loader = "$env:windir\System32\vulkan-1.dll"
    $sha256 = (Get-FileHash -Algorithm SHA256 $loader).Hash.ToLowerInvariant()
    if ($sha256 -ne $pin.Loader) {
        throw "$loader is $((Get-Item $loader).VersionInfo.FileVersion) with SHA-256 $sha256, not the loader of Vulkan SDK $($pin.Version) as packaging/windows/tools.psd1 pins"
    }
    New-Item -ItemType Directory -Force "$sdk\runtime\x64" | Out-Null
    Copy-Item $loader "$sdk\runtime\x64"
    Export VULKAN_SDK $sdk
    Export VULKAN_VERSION $pin.Version
    AddPath "$sdk\Bin"
    Output vulkan $pin.Version
}

# First on PATH, before the runner image's: the cmake that the cmake crate runs.
function CMake {
    $pin = $pins.CMake
    $dir = Join-Path $env:RUNNER_TEMP "cmake-$($pin.Version)"
    New-Item -ItemType Directory -Force $dir | Out-Null
    & "$env:windir\System32\tar.exe" -xf (Fetch CMake) -C $dir
    if ($LASTEXITCODE) { throw "unpacking CMake $($pin.Version) failed" }
    $cmake = Get-ChildItem $dir -Recurse -Filter cmake.exe | Select-Object -First 1
    if (-not $cmake) { throw "CMake $($pin.Version) has no cmake.exe" }
    AddPath $cmake.DirectoryName
    Output cmake $pin.Version
    & $cmake.FullName --version | Select-Object -First 1
}

# Portable: beside the runner image's Inno Setup, registered nowhere; first on PATH as iscc.
function InnoSetup {
    $pin = $pins.InnoSetup
    $dir = Join-Path $env:RUNNER_TEMP "inno-setup-$($pin.Version)"
    Install (Fetch InnoSetup) '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', '/PORTABLE=1', '/CURRENTUSER', "/DIR=`"$dir`"" 5
    if (-not (Test-Path "$dir\ISCC.exe")) { throw "Inno Setup $($pin.Version) installed no ISCC.exe in $dir" }
    AddPath $dir
}

function Sde {
    $pin = $pins.Sde
    $dir = Join-Path $env:RUNNER_TEMP "sde-$($pin.Version)"
    New-Item -ItemType Directory -Force $dir | Out-Null
    & "$env:windir\System32\tar.exe" -xf (Fetch Sde) -C $dir
    if ($LASTEXITCODE) { throw "unpacking Intel SDE $($pin.Version) failed" }
    $sde = Get-ChildItem $dir -Recurse -Filter sde.exe | Select-Object -First 1
    if (-not $sde) { throw "Intel SDE $($pin.Version) has no sde.exe" }
    Export SDE_PATH $sde.DirectoryName
}

foreach ($tool in $Tools) {
    try { & $tool }
    catch {
        Write-Host "::error title=$tool $($pins[$tool].Version)::$("$_" -replace '%', '%25' -replace "`r", '' -replace "`n", '%0A')"
        throw
    }
}
