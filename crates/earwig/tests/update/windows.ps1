# earwig updating itself on Windows, end to end: an older test build, installed with the real
# installer and running, updates to the installer packaging/windows/package.ps1 made, which a
# stand-in for GitHub (fake_github.py) offers. First with a SHA-256 that doesn't match, which it
# refuses, then as released. When packaging/windows/sign.ps1 signs, the older build is signed
# too: it refuses the installer unsigned, signed by another publisher or by a look-alike of its
# own, or changed after signing; and an unsigned older build, as the last release before signing
# was, takes the signed installer. CI runs it after package.ps1, with Inno Setup's iscc at hand.
#   pwsh crates/earwig/tests/update/windows.ps1
# Every wait has a deadline: what outlives it is killed, and the failure says what was running.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$started = Get-Date
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
$sign = "$root\packaging\windows\sign.ps1"
$signed = (& $sign -Mode) -ne 'none'
$state = "$env:LOCALAPPDATA\earwig\state"
$timings = [ordered]@{}
$lap = Get-Date

$transcript = [System.Collections.Generic.List[string]]::new()
# What ran when a wait ran out, taken before the kill.
$stuck = $null

Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Text;
using System.Threading;

/// A console program, its output and errors collected as they come.
public sealed class Bounded
{
    readonly Process process = new Process();
    readonly StringBuilder output = new StringBuilder();
    readonly CountdownEvent ends = new CountdownEvent(2);

    public Bounded(string exe, string[] arguments)
    {
        var info = process.StartInfo;
        info.FileName = exe;
        foreach (var argument in arguments) info.ArgumentList.Add(argument);
        info.UseShellExecute = false;
        info.CreateNoWindow = true;
        info.RedirectStandardInput = true;
        info.RedirectStandardOutput = true;
        info.RedirectStandardError = true;
        DataReceivedEventHandler take = (_, e) =>
        {
            if (e.Data == null) ends.Signal();
            else lock (output) output.AppendLine(e.Data);
        };
        process.OutputDataReceived += take;
        process.ErrorDataReceived += take;
        process.Start();
        process.StandardInput.Close();
        process.BeginOutputReadLine();
        process.BeginErrorReadLine();
    }

    public bool Exited(int seconds) { return process.WaitForExit(seconds * 1000); }

    public int Code { get { return process.ExitCode; } }

    /// It, and every program it started.
    public void Kill() { try { process.Kill(true); } catch (InvalidOperationException) { } }

    /// A program it started kept its output open (it inherited it).
    public bool Held { get; private set; }

    /// What it printed. Its output ends when every process holding it exits, which a program it
    /// started may not do for a while: this takes what came within 5 s of its own exit.
    public string Output()
    {
        Held = !ends.Wait(5000);
        lock (output) return output.ToString().TrimEnd();
    }
}
'@

# The processes this test has to do with: those started since it began, and earwig's and its
# installers' whenever they started; with their parents, start times and command lines.
function Processes {
    (Get-CimInstance Win32_Process | Where-Object { $_.CreationDate -ge $started -or $_.Name -like 'earwig*' } |
        Sort-Object CreationDate | ForEach-Object {
            '{0} {1} (parent {2}, started {3:HH:mm:ss.fff}) {4}' -f $_.ProcessId, $_.Name, $_.ParentProcessId, $_.CreationDate, $_.CommandLine
        }) -join "`n"
}

# A wait ran out: note what runs, kill what $kill kills, fail.
function Stuck([string]$what, [scriptblock]$kill) {
    $script:stuck = Processes
    & $kill
    throw "FAILED: $what"
}

# A program to its end within $seconds, its output and errors together; killed with everything
# it started if it takes longer. Never earwig.exe while an installer runs: it would hold the
# files the installer replaces.
function Run([string]$exe, [string[]]$arguments, [int]$seconds = 60) {
    $command = "$(Split-Path -Leaf $exe) $arguments"
    if ($exe -eq $earwig -and (Get-Process 'earwig-*-setup*' -ErrorAction SilentlyContinue)) {
        Stuck "$command while an installer runs" { }
    }
    $run = [Bounded]::new($exe, $arguments)
    if (-not $run.Exited($seconds)) {
        Stuck "$command did not finish within $seconds s" { $run.Kill() }
    }
    $output = $run.Output()
    $said = "> $command (exit $($run.Code))`n$output"
    if ($run.Held) { $said += "`n(a program it started still holds its output)" }
    Write-Host $said
    $transcript.Add($said)
    @{ Code = $run.Code; Output = $output }
}

function Check([bool]$ok, [string]$what) {
    if (-not $ok) { throw "FAILED: $what" }
    Write-Host "ok: $what"
    $transcript.Add("ok: $what")
}

function Lap([string]$name) {
    $script:timings[$name] = [math]::Round(((Get-Date) - $script:lap).TotalSeconds)
    $script:lap = Get-Date
}

# A failure's details as annotations of the run, which GitHub shows without access to its logs.
function Annotate([string]$level, [string]$title, [string]$text) {
    if (-not $env:GITHUB_ACTIONS) { return }
    $text = if ($text.Length -gt 6000) { $text.Substring($text.Length - 6000) } else { $text }
    Write-Host "::${level} title=${title}::$($text.Replace('%', '%25').Replace("`r", '%0D').Replace("`n", '%0A'))"
}

# Run an installer silently to its end, within $seconds, and return its exit code. Waits on the
# installer alone: `Start-Process -Wait` also waits for the earwig an upgrade starts again.
function Install([string]$setup, [string]$log, [int]$seconds = 180) {
    $installer = Start-Process $setup -PassThru `
        -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$log`""
    $null = $installer.Handle
    if (-not $installer.WaitForExit($seconds * 1000)) {
        Stuck "$(Split-Path -Leaf $setup) did not finish within $seconds s" { $installer.Kill($true) }
    }
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

# The stand-in for GitHub, once it answers, offering $File as the new installer. Its output goes
# to files, not to this step's.
function Serve([switch]$WrongDigest, [string]$File = $newSetup) {
    $arguments = @("`"$PSScriptRoot\fake_github.py`"", '--port', $port, '--version', $version, '--file', "`"$File`"",
        '--name', (Split-Path -Leaf $newSetup))
    if ($WrongDigest) { $arguments += '--wrong-digest' }
    $log = "$work\feed-$(Get-Random).log"
    $feed = Start-Process python -ArgumentList $arguments -PassThru -NoNewWindow `
        -RedirectStandardOutput "$log.out" -RedirectStandardError $log
    try {
        WaitFor {
            if ($feed.HasExited) { throw "FAILED: the stand-in for GitHub exited: $(Get-Content -Raw $log)" }
            try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 "http://127.0.0.1:$port/repos/adeci/earwig/releases/latest" | Out-Null; $true }
            catch { $false }
        } 30 'the stand-in for GitHub answers'
    } catch {
        Stop-Process -Id $feed.Id -ErrorAction SilentlyContinue
        throw
    }
    $feed
}

$feed = $null
$passed = $false
try {
    # The older build: the same code built as the release package is (not for this runner's
    # CPU), an older version, the stand-in compiled in. The lock file gets the same versions, so
    # cargo asks crates.io nothing (--frozen): package.ps1 fetched every crate already.
    $manifests = 'crates\earwig\Cargo.toml', 'crates\earwig-core\Cargo.toml', 'Cargo.lock'
    $saved = @{}
    foreach ($file in $manifests) { $saved[$file] = Get-Content -Raw $file }
    try {
        foreach ($file in $manifests[0..1]) {
            $saved[$file] -replace '(?m)^version\.workspace = true', "version = `"$old`"" | Set-Content -NoNewline $file
        }
        $saved['Cargo.lock'] -replace '(?m)^(name = "earwig(-core)?"\r?\n)version = ".+"', "`${1}version = `"$old`"" |
            Set-Content -NoNewline 'Cargo.lock'
        . "$root\packaging\windows\release-env.ps1"
        $env:EARWIG_UPDATE_TEST_FEED = "http://127.0.0.1:$port"
        $build = Run cargo 'build', '--release', '--frozen', '-p', 'earwig', '-p', 'earwigw' 600
        if ($build.Code) {
            $errors = $build.Output -split "`n" | Where-Object { $_ -notmatch '^\s*(Compiling|Downloaded|Downloading)' }
            throw "building the older test build failed:`n$(($errors | Select-Object -Last 40) -join "`n")"
        }
    } finally {
        Remove-Item Env:EARWIG_UPDATE_TEST_FEED -ErrorAction SilentlyContinue
        foreach ($file in $manifests) { $saved[$file] | Set-Content -NoNewline $file }
    }
    # The package's files, its two programs the older build's.
    Copy-Item "$target\package\earwig-$version-windows-x64\*" "$work\stage"
    Copy-Item -Force "$target\release\earwig.exe", "$target\release\earwigw.exe" "$work\stage"
    $iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
    if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
    if ($signed) {
        # Unsigned too, as the last release before signing was.
        $packaged = Run $iscc '/Q', "/DVersion=$old", "/DStage=$work\stage", "/O$work\unsigned", 'packaging\windows\earwig.iss' 600
        if ($packaged.Code) { throw 'packaging the unsigned older test build failed' }
        & $sign "$work\stage\earwig.exe" "$work\stage\earwigw.exe"
    }
    $packaged = Run $iscc (@('/Q', "/DVersion=$old", "/DStage=$work\stage", "/O$work") + @(& $sign -Iscc) + 'packaging\windows\earwig.iss') 600
    if ($packaged.Code) { throw 'packaging the older test build failed' }
    if ($signed) {
        # The new installer as earwig must not take it: unsigned, signed by another publisher or by
        # a look-alike of its own (the same name and profile EKU, self-signed), changed after signing.
        $bad = "$work\refused"
        New-Item -ItemType Directory $bad | Out-Null
        foreach ($case in 'unsigned', 'other', 'look-alike', 'changed') { Copy-Item $newSetup "$bad\$case.exe" }
        $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
            Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
        if ((Run $signtool.FullName 'remove', '/s', "$bad\unsigned.exe").Code) { throw 'removing the signature failed' }
        & $sign -Publisher other "$bad\other.exe"
        & $sign -Publisher look-alike "$bad\look-alike.exe"
        $bytes = [IO.File]::ReadAllBytes("$bad\changed.exe")
        $middle = $bytes.Length -shr 1
        $bytes[$middle] = $bytes[$middle] -bxor 1
        [IO.File]::WriteAllBytes("$bad\changed.exe", $bytes)
    }
    Lap 'older builds'

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
    if ($signed) { Check ((Get-AuthenticodeSignature $earwig).Status -eq 'Valid') "earwig $old is signed" }
    Check (Sign-In) 'the installer set earwig to start at sign-in'
    # The user then switches Start at sign-in off, which an update must keep.
    Remove-ItemProperty -Path $runKey -Name earwig

    # No microphone on the runner: dictations would hear a clip.
    $env:EARWIG_AUDIO_FILE = "$root\tests\fixtures\quick-brown-fox.wav"
    Start-Process "$app\earwigw.exe"
    WaitFor { (Run $earwig 'status' 20).Code -eq 0 } 60 "earwig $old runs"

    $feed = Serve -WrongDigest
    $check = Run $earwig 'update', '--check'
    Check ($check.Code -eq 0 -and $check.Output.Contains("earwig $version is out")) "earwig $old finds $version"
    $refused = Run $earwig 'update'
    Check ($refused.Code -ne 0 -and $refused.Output.Contains('SHA-256')) 'a download that does not match its digest is refused'
    Check ((Run $earwig '--version').Output -eq "earwig $old") "earwig $old is still installed"
    $status = (Run $earwig 'status' 20).Output | ConvertFrom-Json
    Check ($status.update.state -eq 'failed') 'earwig still runs and says the update failed'
    Check (-not (Test-Path "$env:LOCALAPPDATA\earwig\state\update")) 'nothing of the download is left'
    if ($signed) {
        Remove-Item "$state\install.log" -ErrorAction SilentlyContinue
        foreach ($case in @(
                @{ File = 'unsigned'; Why = 'the installer is not signed'; What = 'an unsigned installer' }
                @{ File = 'other'; Why = 'another certificate profile'; What = 'an installer signed by another publisher' }
                @{ File = 'look-alike'; Why = 'another root certificate'; What = "an installer signed by a look-alike of earwig's publisher" }
                @{ File = 'changed'; Why = "the installer's signature is not valid"; What = 'an installer changed after signing' })) {
            Stop-Process -Id $feed.Id
            $feed = Serve -File "$bad\$($case.File).exe"
            $refused = Run $earwig 'update'
            Check ($refused.Code -ne 0 -and $refused.Output.Contains($case.Why)) "$($case.What) is refused"
            Check ((Run $earwig '--version').Output -eq "earwig $old") "earwig $old is still installed"
            $status = (Run $earwig 'status' 20).Output | ConvertFrom-Json
            Check ($status.update.state -eq 'failed') 'earwig still runs and says the update failed'
            Check (-not (Test-Path "$state\install.log")) 'no installer ran'
            Check (-not (Test-Path "$state\update")) 'nothing of the download is left'
        }
    }
    Lap 'refusals'

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
    WaitFor { (Run $earwig 'status' 20).Code -eq 0 } 60 'earwig runs again after the failed setup'
    Lap 'a failing installer'

    # Back to the older build, by its installer, for an update that goes through.
    Check ((Install "$work\earwig-$old-windows-x64-setup.exe" "$work\install-again.log") -eq 0) "earwig $old installs again"
    Check ((Run $earwig '--version').Output -eq "earwig $old") "earwig $old is installed again"
    WaitFor { (Run $earwig 'status' 20).Code -eq 0 } 60 "earwig $old runs again"
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
    WaitFor { (Run $earwig 'status' 20).Code -eq 0 } 60 "earwig $version runs"
    $running = Get-Process earwig | ForEach-Object { $_.Path }
    Check (@($running | Where-Object { $_ -ne $earwig }).Count -eq 0) "only the installed earwig runs: $running"
    Check (-not (Sign-In)) 'Start at sign-in is still off'
    Lap 'update'

    if ($signed) {
        # The last release before signing, unsigned, takes the signed installer.
        Check ((Install "$work\unsigned\earwig-$old-windows-x64-setup.exe" "$work\install-unsigned.log") -eq 0) "unsigned earwig $old installs"
        Check ((Run $earwig '--version').Output -eq "earwig $old") "unsigned earwig $old is installed"
        Check ((Get-AuthenticodeSignature $earwig).Status -eq 'NotSigned') "earwig $old is not signed"
        WaitFor { (Run $earwig 'status' 20).Code -eq 0 } 60 "unsigned earwig $old runs"
        $updated = Run $earwig 'update'
        Check ($updated.Code -eq 0 -and $updated.Output.Contains("installing earwig $version")) "unsigned earwig $old installs signed $version"
        WaitFor {
            (Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue).DisplayVersion -eq $version -and
            -not (Get-Process | Where-Object { $_.Name -like 'earwig-*-setup*' })
        } 180 "the installer put $version in place"
        Check ((Run $earwig '--version').Output -eq "earwig $version") "earwig $version is installed"
        Check ((Get-AuthenticodeSignature $earwig).Status -eq 'Valid') "earwig $version is signed"
        WaitFor { (Run $earwig 'status' 20).Code -eq 0 } 60 "earwig $version runs"
        Lap 'unsigned to signed'
    }
    Annotate notice 'update end to end' (($transcript | Where-Object { $_ -like 'ok:*' }) -join "`n")
    Annotate notice 'update timings' (($timings.GetEnumerator() | ForEach-Object { "$($_.Key) $($_.Value) s" }) -join ', ')
    $passed = $true
} catch {
    Write-Host $_
    Annotate error 'update end to end' "$_`n$(($transcript | Select-Object -Last 12) -join "`n")"
    $processes = if ($stuck) { $stuck } else { Processes }
    Write-Host "--- processes`n$processes"
    Annotate warning 'processes' $processes
    foreach ($log in @("$env:LOCALAPPDATA\earwig\earwig.log", "$state\install.log", "$work\install-unsigned.log",
            "$work\install-again.log", "$work\install-old.log") + @(Get-ChildItem "$work\feed-*.log" -ErrorAction SilentlyContinue)) {
        if (Test-Path $log) {
            Write-Host "--- $log"
            Get-Content $log -Tail 80 | Write-Host
            Annotate warning (Split-Path -Leaf $log) ((Get-Content $log -Tail 60) -join "`n")
        }
    }
    throw
} finally {
    if ($feed) { Stop-Process -Id $feed.Id -ErrorAction SilentlyContinue }
    if (Test-Path $earwig) {
        try { Run $earwig 'quit' 20 | Out-Null } catch { Write-Host $_ }
    }
    # Nothing this test started outlives it.
    $left = { Get-Process earwig, earwigw, 'earwig-*-setup*' -ErrorAction SilentlyContinue }
    $deadline = (Get-Date).AddSeconds(30)
    while ((& $left) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
    if (& $left) {
        $processes = Processes
        & $left | Stop-Process -Force
        if ($passed) {
            Annotate error 'update end to end' "still running 30 s after ``earwig quit``:`n$processes"
            throw "FAILED: earwig quits:`n$processes"
        }
    }
}
