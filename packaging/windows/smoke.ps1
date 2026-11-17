# The packaged earwig end to end on a desktop session (CI's windows runner):
# the installer's version info; silent install; the installer and what it
# installed signed as sign.ps1 signs; start as sign-in does with
# EARWIG_AUDIO_FILE; a held and a tapped Ctrl+Space into Notepad through the
# keyboard hook; the engine on other CPUs under Intel SDE ($env:SDE_PATH);
# uninstall while running.
#   pwsh packaging/windows/smoke.ps1   (after package.ps1)
# Fails with ::error:: annotations; every wait is bounded.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Resolve-Path "$PSScriptRoot\..\.."
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$root\target" }
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
$setup = Get-ChildItem "$target\package\*-setup.exe" | Select-Object -First 1
$app = "$env:LOCALAPPDATA\Programs\earwig"
$earwig = "$app\earwig.exe"
$clip = "$root\tests\fixtures\quick-brown-fox.wav"
$sentence = 'The quick brown fox jumps over the lazy dog'
$script:failures = 0
$script:timings = [ordered]@{}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class Desk
{
    [StructLayout(LayoutKind.Sequential)]
    struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)]
    struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Explicit)]
    struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)]
    struct INPUT { public uint type; public InputUnion u; }
    [StructLayout(LayoutKind.Sequential)]
    struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")] static extern bool MoveWindow(IntPtr hwnd, int x, int y, int w, int h, bool repaint);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr hwnd, StringBuilder text, int max);
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr SendMessageTimeoutW(IntPtr hwnd, uint msg, IntPtr wparam, StringBuilder lparam, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll")]
    static extern IntPtr SendMessageTimeoutW(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam, uint flags, uint timeout, out IntPtr result);

    public const ushort Ctrl = 0x1d, Space = 0x39;

    static void Send(INPUT[] inputs)
    {
        if (SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(INPUT))) != inputs.Length)
            throw new InvalidOperationException("SendInput failed: " + Marshal.GetLastWin32Error());
    }

    /// One key edge by scan code, as a keyboard sends it.
    public static void Key(ushort scan, bool up)
    {
        var input = new INPUT { type = 1 };
        input.u.ki = new KEYBDINPUT { wScan = scan, dwFlags = 8u | (up ? 2u : 0u) }; // KEYEVENTF_SCANCODE, KEYUP
        Send(new[] { input });
    }

    static string ClassOf(IntPtr hwnd)
    {
        var text = new StringBuilder(256);
        GetClassNameW(hwnd, text, text.Capacity);
        return text.ToString();
    }

    /// The first visible top-level window of class `cls`, or zero.
    public static IntPtr Window(string cls)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hwnd, _) =>
        {
            if (IsWindowVisible(hwnd) && ClassOf(hwnd) == cls) { found = hwnd; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static IntPtr Edit(IntPtr top)
    {
        IntPtr found = IntPtr.Zero;
        EnumChildWindows(top, (hwnd, _) =>
        {
            string cls = ClassOf(hwnd);
            if (cls == "Edit" || cls.StartsWith("RichEdit")) { found = hwnd; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static string Text(IntPtr hwnd)
    {
        IntPtr length, copied;
        SendMessageTimeoutW(hwnd, 0x000E, IntPtr.Zero, IntPtr.Zero, 2, 5000, out length); // WM_GETTEXTLENGTH
        var text = new StringBuilder(length.ToInt32() + 1);
        SendMessageTimeoutW(hwnd, 0x000D, new IntPtr(text.Capacity), text, 2, 5000, out copied); // WM_GETTEXT
        return text.ToString();
    }

    /// Place `hwnd` and bring it to the front: ask, then click its title bar as a user would.
    public static bool Focus(IntPtr hwnd)
    {
        ShowWindow(hwnd, 9);
        MoveWindow(hwnd, 60, 60, 760, 420, true);
        SetForegroundWindow(hwnd);
        Thread.Sleep(200);
        if (GetForegroundWindow() == hwnd) return true;
        RECT r;
        GetWindowRect(hwnd, out r);
        SetCursorPos((r.Left + r.Right) / 2, r.Top + 12);
        var down = new INPUT { type = 0 };
        down.u.mi = new MOUSEINPUT { dwFlags = 2 };
        var up = new INPUT { type = 0 };
        up.u.mi = new MOUSEINPUT { dwFlags = 4 };
        Send(new[] { down, up });
        Thread.Sleep(300);
        return GetForegroundWindow() == hwnd;
    }
}
'@

function Fail([string]$what) {
    $script:failures++
    Write-Host "::error title=smoke::$($what -replace '%', '%25' -replace "`r", '' -replace "`n", '%0A')"
}

function Step([string]$name, [scriptblock]$body) {
    Write-Host "== $name"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & $body
    $script:timings[$name] = [math]::Round($watch.Elapsed.TotalSeconds, 1)
}

# A console program without a window: exit code, stdout, stderr; killed after $timeout s. Its
# output ends when every process holding it exits, which a program it started may not do: what
# came within 5 s of its own exit is what it printed.
function Run([string]$exe, [string[]]$arguments, [int]$timeout = 60) {
    $info = [Diagnostics.ProcessStartInfo]::new($exe)
    foreach ($a in $arguments) { $info.ArgumentList.Add($a) }
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $p = [Diagnostics.Process]::Start($info)
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    $code = if ($p.WaitForExit($timeout * 1000)) { $p.ExitCode } else { $p.Kill($true); 'timeout' }
    try { $null = [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($out, $err), 5000) } catch { }
    [pscustomobject]@{
        Code = $code
        Out = if ($out.IsCompletedSuccessfully) { $out.Result.Trim() } else { '' }
        Err = if ($err.IsCompletedSuccessfully) { $err.Result.Trim() } else { '' }
    }
}

function WaitFor([scriptblock]$condition, [int]$timeout) {
    $deadline = (Get-Date).AddSeconds($timeout)
    do {
        $value = & $condition
        if ($value) { return $value }
        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)
    $null
}

function Status {
    $r = Run $earwig @('status') 15
    if ($r.Code -eq 0) { $r.Out | ConvertFrom-Json }
}

function Running { @(Get-Process earwig, earwigw -ErrorAction SilentlyContinue) }

# `earwig events` into a file (through cmd, so nothing has to pump it here).
function StartEvents([string]$file) {
    $info = [Diagnostics.ProcessStartInfo]::new('cmd.exe', "/d /c `"`"$earwig`" events > `"$file`"`"")
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $p = [Diagnostics.Process]::Start($info)
    WaitFor { (Test-Path $file) -and (Get-Item $file).Length -gt 0 } 30 | Out-Null
    $p
}

function Events([string]$file) {
    $stream = [IO.File]::Open($file, 'Open', 'Read', 'ReadWrite')
    $text = [IO.StreamReader]::new($stream).ReadToEnd()
    $stream.Dispose()
    @($text -split "`r?`n" | Where-Object { $_ } | ForEach-Object { try { $_ | ConvertFrom-Json } catch { } })
}

# The first event after index $from that $match accepts, within $timeout s.
function WaitEvent([string]$file, [int]$from, [scriptblock]$match, [int]$timeout) {
    WaitFor { Events $file | Select-Object -Skip $from | Where-Object { & $match $_ } | Select-Object -First 1 } $timeout
}

function States([string]$file, [int]$from) {
    (Events $file | Select-Object -Skip $from | Where-Object { $_.event -in 'state', 'inserted', 'insert-failed', 'error' } |
        ForEach-Object { $_ | ConvertTo-Json -Compress -Depth 4 }) -join ' '
}

# Windows shows the file version, digits only: the version without its pre-release suffix. Inno
# Setup pads the version strings with spaces.
Step 'installer version' {
    $info = $setup.VersionInfo
    $file = '{0}.{1}.{2}.{3}' -f $info.FileMajorPart, $info.FileMinorPart, $info.FileBuildPart, $info.FilePrivatePart
    $text = $info.FileVersion.Trim()
    $product = $info.ProductVersion.Trim()
    Write-Host "::notice title=installer version::$($setup.Name): file version $file ($text), product version $product"
    if ($file -ne "$(($version -split '-')[0]).0" -or $text -ne $version -or $product -ne $version) {
        Fail "$($setup.Name) has file version $file ($text) and product version $product, not those of $version"
    }
}

$signIn = $null
Step 'install' {
    $p = Start-Process $setup.FullName -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    if ($p.ExitCode -ne 0) { Fail "the installer exited with $($p.ExitCode)" }
    $script:signIn = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run').earwig
    if ($script:signIn -ne "`"$app\earwigw.exe`"") { Fail "start at sign-in is '$script:signIn'" }
}

Step 'signatures' {
    try { & "$root\packaging\windows\sign.ps1" -Check $setup.FullName "$app\earwig.exe" "$app\earwigw.exe" "$app\unins000.exe" }
    catch { Fail "signatures: $_" }
}

Step 'model' {
    # A model CI restored from its cache must have the catalog's SHA-256, as a download must.
    $entry = [regex]::Match((Get-Content -Raw "$root\crates\earwig-core\models.toml"), '(?s)id = "parakeet-unified-en-0\.6b".*?(?=\[\[|\z)').Value
    $field = { param($name) [regex]::Match($entry, "(?m)^$name = `"([^`"]+)`"").Groups[1].Value }
    $cached = "$env:LOCALAPPDATA\earwig\models\$(& $field repo)\$(& $field revision)\$(& $field file)"
    if ((Test-Path $cached) -and (Get-FileHash -Algorithm SHA256 $cached).Hash -ne (& $field sha256)) {
        Write-Host "::warning title=model::$cached is not the catalog's: downloading it again"
        Remove-Item $cached
    }
    $list = Run $earwig @('models', 'list')
    if ($list.Out -notmatch '\*\s+parakeet-unified-en-0\.6b\s+.*installed') {
        $download = Run $earwig @('models', 'download', 'parakeet-unified-en-0.6b') 900
        if ($download.Code -ne 0) { Fail "earwig models download: $($download.Err)" }
    }
}

# Other CPUs, emulated: Haswell (AVX2) transcribes; Ivy Bridge (AVX, no AVX2) gets a plain error.
if ($env:SDE_PATH) {
    $sde = (Get-ChildItem $env:SDE_PATH -Recurse -Filter sde.exe | Select-Object -First 1).FullName
    Step 'sde' {
        $hsw = Run $sde @('-hsw', '--', $earwig, 'transcribe', $clip) 300
        if ($hsw.Code -ne 0 -or $hsw.Out -notmatch $sentence) { Fail "on an emulated Haswell: exit $($hsw.Code) '$($hsw.Out)' $($hsw.Err)" }
        $version = Run $sde @('-ivb', '--', $earwig, '--version') 120
        $ivb = Run $sde @('-ivb', '--', $earwig, 'transcribe', $clip) 300
        Write-Host "::notice title=Ivy Bridge (no AVX2)::earwig --version: exit $($version.Code) '$($version.Out)'%0Aearwig transcribe: exit $($ivb.Code) '$($ivb.Err -replace "`r?`n", ' ')'"
        if ($ivb.Code -eq 0 -or $ivb.Err -notmatch 'lacks AVX2' -or "$($ivb.Out)$($ivb.Err)" -match 'SDE-ERROR') {
            Fail "on an emulated Ivy Bridge: exit $($ivb.Code) $($ivb.Err)"
        }
        # The daemon there starts and reports why the model can't load.
        $daemon = Start-Process $sde -ArgumentList '-ivb', '--', $earwig, 'daemon', '--headless' -WindowStyle Hidden -PassThru
        $failed = WaitFor { if ($daemon.HasExited) { 'exited' } else { $s = Status; if ($s.model.state -eq 'failed') { $s } } } 180
        Write-Host "::notice title=Ivy Bridge daemon::$(if ($failed -eq 'exited') { "exited with $($daemon.ExitCode)" } else { $failed.model | ConvertTo-Json -Compress })"
        if ($failed.model.message -notmatch 'lacks AVX2') { Fail "on an emulated Ivy Bridge the daemon: $failed $(Status | ConvertTo-Json -Compress -Depth 4)" }
        Run $earwig @('quit') | Out-Null
        if (-not (WaitFor { $daemon.HasExited } 60)) {
            Fail 'the emulated daemon did not quit'
            Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
        }
    }
}

$notepad = $null
Step 'dictation' {
    $env:EARWIG_AUDIO_FILE = $clip
    Start-Process $signIn.Trim('"')
    $ready = WaitFor { $s = Status; if ($s.model.state -eq 'ready' -and $s.hotkey.keyboard -eq 'watching') { $s } } 120
    if (-not $ready) { Fail "earwig did not get ready: $(Status | ConvertTo-Json -Compress -Depth 4)"; return }
    $file = "$env:RUNNER_TEMP\earwig-events.jsonl"
    $events = StartEvents $file
    $script:notepad = Start-Process notepad.exe -PassThru
    $window = WaitFor { $w = [Desk]::Window('Notepad'); if ($w -ne [IntPtr]::Zero) { $w } } 30
    $edit = if ($window) { [Desk]::Edit($window) } else { [IntPtr]::Zero }
    if (-not $window -or $edit -eq [IntPtr]::Zero -or -not [Desk]::Focus($window)) { Fail 'Notepad did not get the focus'; return }
    $inserted = @()

    # Hold over the clip, the key repeating as a keyboard's does.
    $mark = @(Events $file).Count
    [Desk]::Key([Desk]::Ctrl, $false)
    Start-Sleep -Milliseconds 40
    [Desk]::Key([Desk]::Space, $false)
    if (-not (WaitEvent $file $mark { param($e) $e.event -eq 'state' -and $e.state.state -eq 'recording' -and -not $e.state.latched } 10)) {
        Fail "hold: the press started no recording: $(States $file $mark)"
    }
    Start-Sleep -Milliseconds 500
    for ($i = 0; $i -lt 45; $i++) { [Desk]::Key([Desk]::Space, $false); Start-Sleep -Milliseconds 33 }
    Start-Sleep -Milliseconds 2500
    [Desk]::Key([Desk]::Space, $true)
    Start-Sleep -Milliseconds 40
    [Desk]::Key([Desk]::Ctrl, $true)
    $result = WaitEvent $file $mark { param($e) $e.event -in 'inserted', 'insert-failed' } 60
    if ($result.event -ne 'inserted' -or $result.text -notmatch 'brown fox') { Fail "hold: $(States $file $mark)" } else { $inserted += $result.text }
    WaitEvent $file $mark { param($e) $e.event -eq 'state' -and $e.state.state -eq 'idle' } 30 | Out-Null

    # Tap to start, tap to stop.
    $mark = @(Events $file).Count
    foreach ($tap in 1, 2) {
        [Desk]::Key([Desk]::Ctrl, $false)
        Start-Sleep -Milliseconds 40
        [Desk]::Key([Desk]::Space, $false)
        Start-Sleep -Milliseconds 80
        [Desk]::Key([Desk]::Space, $true)
        Start-Sleep -Milliseconds 40
        [Desk]::Key([Desk]::Ctrl, $true)
        if ($tap -eq 1) {
            if (-not (WaitEvent $file $mark { param($e) $e.event -eq 'state' -and $e.state.latched } 10)) { Fail "tap: no latched recording: $(States $file $mark)" }
            Start-Sleep -Milliseconds 4000
        }
    }
    $result = WaitEvent $file $mark { param($e) $e.event -in 'inserted', 'insert-failed' } 60
    if ($result.event -ne 'inserted' -or $result.text -notmatch 'brown fox') { Fail "tap: $(States $file $mark)" } else { $inserted += $result.text }
    Start-Sleep -Milliseconds 500
    $text = [Desk]::Text($edit)
    # The hotkey itself must never reach Notepad (a leaked Space would show).
    if ($text -ne ($inserted -join '')) { Fail "Notepad holds '$text', not the transcripts '$($inserted -join '')'" }
    if ($events -and -not $events.HasExited) { $events.Kill($true) }
    Remove-Item Env:\EARWIG_AUDIO_FILE
}
if ($notepad) { Stop-Process -Id $notepad.Id -ErrorAction SilentlyContinue }

Step 'uninstall while running' {
    if (-not (Status)) {
        Start-Process $signIn.Trim('"')
        WaitFor { Status } 60 | Out-Null
    }
    $log = "$env:RUNNER_TEMP\earwig-uninstall.log"
    Start-Process "$app\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$log`"" -Wait
    # The uninstaller hands over to a copy of itself in %TEMP%.
    WaitFor { -not (Get-Process '_iu*' -ErrorAction SilentlyContinue) -and (Select-String -Path $log -Pattern 'Log closed' -Quiet -ErrorAction SilentlyContinue) } 60 | Out-Null
    $left = @(Get-ChildItem $app -Recurse -Force -ErrorAction SilentlyContinue | ForEach-Object FullName)
    if (Running) { Fail "still running after uninstall: $((Running).Name -join ', ')" }
    if ((Test-Path $app) -or -not (Select-String -Path $log -Pattern 'Removed all\? Yes' -Quiet -ErrorAction SilentlyContinue)) {
        Fail "uninstall left $($left -join ', '): $((Select-String -Path $log -Pattern 'Failed|Removed all' -ErrorAction SilentlyContinue).Line -join ' | ')"
    }
}

$summary = ($script:timings.GetEnumerator() | ForEach-Object { "$($_.Key) $($_.Value) s" }) -join ', '
Write-Host "::notice title=smoke timings::$summary"
if ($script:failures) { exit 1 }
