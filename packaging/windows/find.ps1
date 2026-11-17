# Where the programs earwig's Windows scripts run are, found one way for all of them: dot-sourced
# by package.ps1, sign.ps1 and the update test.

# The newest Windows SDK's signtool.exe.
function SignTool {
    $found = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
    if (-not $found) { throw 'signtool.exe not found: install the Windows SDK' }
    $found
}

# Inno Setup's compiler: the first iscc on PATH (CI's, from tools.ps1), else where its installer
# puts it; nothing when there is none.
function Iscc {
    $found = (Get-Command iscc -ErrorAction SilentlyContinue).Source
    if (-not $found) {
        $found = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
            Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    $found
}
