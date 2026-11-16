# Build earwig for Windows and package it: a portable zip and, when Inno
# Setup's iscc is on PATH (or in its default folder), a per-user installer;
# signed as sign.ps1 decides (Artifact Signing, CI's test publisher, or not).
#   pwsh packaging/windows/package.ps1
# Output: target/package/earwig-<version>-windows-x64.zip and -setup.exe.
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value

# Pinned CPU tiers, the C runtime linked in, the update marker, no test feed.
. "$PSScriptRoot\release-env.ps1"
cargo build --release --locked -p earwig -p earwigw
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$root\target" }
& "$PSScriptRoot\sign.ps1" "$target\release\earwig.exe" "$target\release\earwigw.exe"
$out = "$target\package"
$name = "earwig-$version-windows-x64"
$stage = "$out\$name"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$target\release\earwig.exe", "$target\release\earwigw.exe", LICENSE, README.md,
    assets\fonts\LICENSE-IBM-Plex-Sans.txt, packaging\windows\LICENSE-Vulkan-Loader.txt $stage
# The Vulkan loader, which GPU drivers install: without it Windows won't start
# earwig.exe at all. With no driver behind it, earwig runs on the CPU. The
# SDK's copy (CI installs the runtime into it), else the system's.
$loader = @("$env:VULKAN_SDK\runtime\x64\vulkan-1.dll", "$env:windir\System32\vulkan-1.dll") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $loader) { throw 'vulkan-1.dll not found: install the Vulkan SDK with its runtime' }
Copy-Item $loader $stage

# The DLLs a PE image imports (its import directory).
function Imports([string]$path) {
    $b = [IO.File]::ReadAllBytes($path)
    $pe = [BitConverter]::ToInt32($b, 0x3c)
    $optional = $pe + 24
    $dirs = $optional + $(if ([BitConverter]::ToUInt16($b, $optional) -eq 0x20b) { 112 } else { 96 })
    $sections = $optional + [BitConverter]::ToUInt16($b, $pe + 20)
    $offset = {
        param([uint32]$rva)
        for ($s = $sections; $s -lt $sections + 40 * [BitConverter]::ToUInt16($b, $pe + 6); $s += 40) {
            $va = [BitConverter]::ToUInt32($b, $s + 12)
            if ($rva -ge $va -and $rva -lt $va + [BitConverter]::ToUInt32($b, $s + 8)) {
                return [int]($rva - $va + [BitConverter]::ToUInt32($b, $s + 20))
            }
        }
    }
    for ($d = & $offset ([BitConverter]::ToUInt32($b, $dirs + 8)); [BitConverter]::ToUInt32($b, $d + 12); $d += 20) {
        $name = & $offset ([BitConverter]::ToUInt32($b, $d + 12))
        [Text.Encoding]::ASCII.GetString($b, $name, [Array]::IndexOf($b, [byte]0, $name) - $name)
    }
}

# Every DLL the binaries import comes with Windows (Windows Resource Protection
# gives its files to TrustedInstaller) or with the package.
foreach ($exe in 'earwig.exe', 'earwigw.exe') {
    $foreign = Imports "$stage\$exe" | Where-Object {
        $_ -notmatch '^(api|ext)-ms-' -and -not (Test-Path "$stage\$_") -and
        (Get-Acl "$env:windir\System32\$_" -ErrorAction SilentlyContinue).Owner -ne 'NT SERVICE\TrustedInstaller'
    }
    if ($foreign) { throw "$exe imports $($foreign -join ', '), which Windows doesn't come with" }
}

Compress-Archive -Force -Path "$stage\*" -DestinationPath "$out\$name.zip"
Write-Output "$out\$name.zip"

$iscc = Get-Command iscc -ErrorAction SilentlyContinue
if (-not $iscc) {
    $iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iscc) {
    Write-Warning 'Inno Setup (iscc) not found: no installer'
    exit 0
}
# Inno Setup signs Setup and its uninstaller with sign.ps1 too.
& $iscc /Q "/DVersion=$version" "/DStage=$stage" "/O$out" @(& "$PSScriptRoot\sign.ps1" -Iscc) packaging\windows\earwig.iss
if ($LASTEXITCODE) { exit $LASTEXITCODE }
& "$PSScriptRoot\sign.ps1" -Check "$out\$name-setup.exe"
Write-Output "$out\$name-setup.exe"

# winget manifests for the installer as a GitHub release asset of v<version>,
# ready for a pull request to microsoft/winget-pkgs (manifests/a/adeci/earwig).
$setup = "$out\$name-setup.exe"
$sha = (Get-FileHash -Algorithm SHA256 $setup).Hash
$manifests = "$out\winget\$version"
New-Item -ItemType Directory -Force $manifests | Out-Null
$schema = '1.9.0'
@"
PackageIdentifier: adeci.earwig
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $schema
"@ | Set-Content -Encoding utf8 "$manifests\adeci.earwig.yaml"
@"
PackageIdentifier: adeci.earwig
PackageVersion: $version
InstallerType: inno
Scope: user
InstallModes: [interactive, silent, silentWithProgress]
UpgradeBehavior: install
Installers:
- Architecture: x64
  InstallerUrl: https://github.com/adeci/earwig/releases/download/v$version/$name-setup.exe
  InstallerSha256: $sha
ManifestType: installer
ManifestVersion: $schema
"@ | Set-Content -Encoding utf8 "$manifests\adeci.earwig.installer.yaml"
@"
PackageIdentifier: adeci.earwig
PackageVersion: $version
PackageLocale: en-US
Publisher: adeci
PackageName: earwig
License: MIT
ShortDescription: Local dictation. Hold a key and talk; the text lands where you are typing.
PackageUrl: https://github.com/adeci/earwig
ManifestType: defaultLocale
ManifestVersion: $schema
"@ | Set-Content -Encoding utf8 "$manifests\adeci.earwig.locale.en-US.yaml"
Write-Output $manifests
