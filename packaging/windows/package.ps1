# Build earwig for Windows and package it: a portable zip and, when Inno
# Setup's iscc is on PATH (or in its default folder), a per-user installer.
#   pwsh packaging/windows/package.ps1
# Output: target/package/earwig-<version>-windows-x64.zip and -setup.exe.
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value

cargo build --release --locked -p earwig -p earwigw
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$root\target" }
$out = "$target\package"
$name = "earwig-$version-windows-x64"
$stage = "$out\$name"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$target\release\earwig.exe", "$target\release\earwigw.exe", LICENSE, README.md $stage
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
& $iscc /Q "/DVersion=$version" "/DStage=$stage" "/O$out" packaging\windows\earwig.iss
if ($LASTEXITCODE) { exit $LASTEXITCODE }
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
