# earwig updating itself on Windows, end to end: an older test build, installed with the real
# installer and running, updates to the installer packaging/windows/package.ps1 made, which a
# stand-in for GitHub (fake_github.py) offers. First with a SHA-256 that doesn't match, which it
# refuses, then as released. CI runs it after package.ps1, with Inno Setup's iscc at hand.
#   pwsh crates/earwig/tests/update/windows.ps1
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\..\..\.."
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$root\target" }
$newSetup = "$target\package\earwig-$version-windows-x64-setup.exe"
if (-not (Test-Path $newSetup)) { throw "no ${newSetup}: run packaging/windows/package.ps1 first" }
# Older than the release; a pre-release itself when the release is one, which a stable build
# would not take.
$old = if ($version.Contains('-')) { '0.0.1-test' } else { '0.0.1' }
$port = 8749
$work = "$target\update-test"
$app = "$env:LOCALAPPDATA\Programs\earwig"
$earwig = "$app\earwig.exe"
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{6A3C4F2E-8F1B-4C57-9E0D-2B7E5A1D9C41}_is1'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$work\stage" | Out-Null

$transcript = [System.Collections.Generic.List[string]]::new()

function Run([string]$exe, [string[]]$arguments) {
    $ErrorActionPreference = 'Continue'
    $output = (& $exe @arguments 2>&1 | ForEach-Object { "$_" }) -join "`n"
    $code = $LASTEXITCODE
    $said = "> $(Split-Path -Leaf $exe) $arguments (exit $code)`n$output"
    Write-Host $said
    $transcript.Add($said)
    @{ Code = $code; Output = $output }
}

function Check([bool]$ok, [string]$what) {
    if (-not $ok) { throw "FAILED: $what" }
    Write-Host "ok: $what"
    $transcript.Add("ok: $what")
}

# A failure's details as annotations of the run, which GitHub shows without access to its logs.
function Annotate([string]$level, [string]$title, [string]$text) {
    if (-not $env:GITHUB_ACTIONS) { return }
    $text = if ($text.Length -gt 6000) { $text.Substring($text.Length - 6000) } else { $text }
    Write-Host "::${level} title=${title}::$($text.Replace('%', '%25').Replace("`r", '%0D').Replace("`n", '%0A'))"
}

# Run an installer silently to its end and return its exit code. Not `Start-Process -Wait`,
# which also waits for the earwig an upgrade starts again.
function Install([string]$setup, [string]$log) {
    $installer = Start-Process $setup -PassThru `
        -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$log`""
    $null = $installer.Handle
    $installer.WaitForExit()
    $installer.ExitCode
}

function WaitFor([scriptblock]$done, [int]$seconds, [string]$what) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while (-not (& $done)) {
        if ((Get-Date) -gt $deadline) { throw "FAILED: $what within $seconds s" }
        Start-Sleep -Milliseconds 500
    }
    Write-Host "ok: $what"
}

function Sign-In { $null -ne (Get-ItemProperty -Path $runKey -Name earwig -ErrorAction SilentlyContinue) }

function Serve([switch]$WrongDigest) {
    $arguments = @("`"$PSScriptRoot\fake_github.py`"", '--port', $port, '--version', $version, '--file', "`"$newSetup`"")
    if ($WrongDigest) { $arguments += '--wrong-digest' }
    $log = "$work\feed-$(Get-Random).log"
    $feed = Start-Process python -ArgumentList $arguments -PassThru -NoNewWindow -RedirectStandardError $log
    WaitFor {
        try { Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$port/repos/adeci/earwig/releases/latest" | Out-Null; $true }
        catch { $false }
    } 30 'the stand-in for GitHub answers'
    $feed
}

$feed = $null
try {
    # The older build: the same code built as the release package is (not for this runner's
    # CPU), an older version, the stand-in compiled in.
    $manifests = 'crates\earwig\Cargo.toml', 'crates\earwig-core\Cargo.toml', 'Cargo.lock'
    $saved = @{}
    foreach ($file in $manifests) { $saved[$file] = Get-Content -Raw $file }
    try {
        foreach ($file in $manifests[0..1]) {
            $saved[$file] -replace '(?m)^version\.workspace = true', "version = `"$old`"" | Set-Content -NoNewline $file
        }
        . "$root\packaging\windows\release-env.ps1"
        $env:EARWIG_UPDATE_TEST_FEED = "http://127.0.0.1:$port"
        $build = Run cargo 'build', '--release', '-p', 'earwig', '-p', 'earwigw'
        if ($build.Code) {
            $errors = $build.Output -split "`n" | Where-Object { $_ -notmatch '^\s*(Compiling|Downloaded|Downloading)' }
            throw "building the older test build failed:`n$(($errors | Select-Object -Last 40) -join "`n")"
        }
    } finally {
        Remove-Item Env:EARWIG_UPDATE_TEST_FEED -ErrorAction SilentlyContinue
        foreach ($file in $manifests) { $saved[$file] | Set-Content -NoNewline $file }
    }
    Copy-Item "$target\release\earwig.exe", "$target\release\earwigw.exe",
        "$target\package\earwig-$version-windows-x64\vulkan-1.dll" "$work\stage"
    $iscc = Get-Command iscc -ErrorAction SilentlyContinue
    if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
    & $iscc /Q "/DVersion=$old" "/DStage=$work\stage" "/O$work" packaging\windows\earwig.iss
    if ($LASTEXITCODE) { throw 'packaging the older test build failed' }

    # Whatever an earlier step (the installer's smoke test) left: no earwig running, none
    # installed. The uninstaller hands over to a copy of itself, so its own end says nothing.
    Get-Process earwig, earwigw -ErrorAction SilentlyContinue | Stop-Process -Force
    $previous = Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue
    if ($previous) {
        Start-Process $previous.UninstallString.Trim('"') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
        WaitFor { -not (Test-Path -LiteralPath $uninstallKey) } 120 'the earlier install is gone'
    }
    Check ((Install "$work\earwig-$old-windows-x64-setup.exe" "$work\install-old.log") -eq 0) "earwig $old installs"
    Check ((Run $earwig '--version').Output -eq "earwig $old") "earwig $old is installed"
    Check (Sign-In) 'the installer set earwig to start at sign-in'
    # The user then switches Start at sign-in off, which an update must keep.
    Remove-ItemProperty -Path $runKey -Name earwig

    # No microphone on the runner: dictations would hear a clip.
    $env:EARWIG_AUDIO_FILE = "$root\tests\fixtures\quick-brown-fox.wav"
    Start-Process "$app\earwigw.exe"
    WaitFor { (Run $earwig 'status').Code -eq 0 } 60 "earwig $old runs"

    $feed = Serve -WrongDigest
    $check = Run $earwig 'update', '--check'
    Check ($check.Code -eq 0 -and $check.Output.Contains("earwig $version is out")) "earwig $old finds $version"
    $refused = Run $earwig 'update'
    Check ($refused.Code -ne 0 -and $refused.Output.Contains('SHA-256')) 'a download that does not match its digest is refused'
    Check ((Run $earwig '--version').Output -eq "earwig $old") "earwig $old is still installed"
    $status = (Run $earwig 'status').Output | ConvertFrom-Json
    Check ($status.update.state -eq 'failed') 'earwig still runs and says the update failed'
    Check (-not (Test-Path "$env:LOCALAPPDATA\earwig\state\update")) 'nothing of the download is left'

    Stop-Process -Id $feed.Id
    $feed = Serve
    # A setup that stopped earwig and then fails (a file it must replace is held) gives up, and
    # earwig runs again. Inno's rollback keeps the files it had already replaced, so that earwig
    # may be the new one. Waits on the installer, not on earwig.exe: running that now would hold
    # the very file the installer replaces.
    $held = [System.IO.File]::Open("$app\README.md", 'Open', 'Read', 'None')
    try {
        $failing = Run $earwig 'update'
        Check ($failing.Code -eq 0 -and $failing.Output.Contains("installing earwig $version")) 'the installer starts'
        Start-Sleep -Seconds 2
        WaitFor { -not (Get-Process | Where-Object { $_.Name -like 'earwig-*-setup*' }) } 180 'the installer gives up'
    } finally {
        $held.Close()
    }
    Check ((Get-ItemProperty -LiteralPath $uninstallKey).DisplayVersion -eq $old) "earwig $old is still registered"
    WaitFor { (Run $earwig 'status').Code -eq 0 } 60 'earwig runs again after the failed setup'

    # Back to the older build, by its installer, for an update that goes through.
    Check ((Install "$work\earwig-$old-windows-x64-setup.exe" "$work\install-again.log") -eq 0) "earwig $old installs again"
    Check ((Run $earwig '--version').Output -eq "earwig $old") "earwig $old is installed again"
    WaitFor { (Run $earwig 'status').Code -eq 0 } 60 "earwig $old runs again"
    Check (-not (Sign-In)) 'a silent reinstall leaves Start at sign-in off'

    $updated = Run $earwig 'update'
    Check ($updated.Code -eq 0 -and $updated.Output.Contains("installing earwig $version")) "earwig $old installs $version"
    # Waits on the installer and the registry, not on earwig.exe: running it now would hold the
    # file the installer replaces.
    WaitFor {
        (Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue).DisplayVersion -eq $version -and
        -not (Get-Process | Where-Object { $_.Name -like 'earwig-*-setup*' })
    } 180 "the installer put $version in place"
    Check ((Run $earwig '--version').Output -eq "earwig $version") "earwig $version is installed"
    WaitFor { (Run $earwig 'status').Code -eq 0 } 60 "earwig $version runs"
    $running = Get-Process earwig | ForEach-Object { $_.Path }
    Check (@($running | Where-Object { $_ -ne $earwig }).Count -eq 0) "only the installed earwig runs: $running"
    Check (-not (Sign-In)) 'Start at sign-in is still off'
} catch {
    Write-Host $_
    Annotate error 'update end to end' "$_`n$(($transcript | Select-Object -Last 12) -join "`n")"
    foreach ($log in "$env:LOCALAPPDATA\earwig\earwig.log", "$env:LOCALAPPDATA\earwig\state\install.log",
        "$work\install-again.log", "$work\install-old.log") {
        if (Test-Path $log) {
            Write-Host "--- $log"
            Get-Content $log -Tail 80 | Write-Host
            Annotate warning (Split-Path -Leaf $log) ((Get-Content $log -Tail 60) -join "`n")
        }
    }
    Get-ChildItem "$work\feed-*.log" -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "--- $_"; Get-Content $_ -Tail 40 | Write-Host
    }
    throw
} finally {
    if ($feed) { Stop-Process -Id $feed.Id -ErrorAction SilentlyContinue }
    if (Test-Path $earwig) { Run $earwig 'quit' | Out-Null }
}
