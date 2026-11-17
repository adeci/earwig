using namespace System.Security.Cryptography
using namespace System.Security.Cryptography.X509Certificates
# Signs earwig's Windows programs and installers, or checks how they are signed, as every job of a
# run decides alike from its ref:
#   artifact-signing  Azure Artifact Signing, on a v* tag in GitHub Actions: SignTool with Microsoft's
#                     dlib (tools.ps1 ArtifactSigning puts it in ARTIFACT_SIGNING_DLIB), signed in
#                     with the job's OIDC token (permissions: id-token: write) as the app registration
#                     AZURE_TENANT_ID and AZURE_CLIENT_ID name, to the profile
#                     ARTIFACT_SIGNING_ENDPOINT, _ACCOUNT and _PROFILE name. Anything but -Check
#                     fails unless all five are set: a tag is never signed otherwise, nor released
#                     unsigned.
#   test              elsewhere in GitHub Actions, so the signed path is always tested: a throwaway
#                     publisher made on the runner, its root trusted there only, with a new
#                     certificate for every signing and the same certificate profile EKU, as
#                     Artifact Signing's certificates are.
#   none              outside GitHub Actions: nothing.
# Signatures are SHA-256, timestamped by Artifact Signing's TSA.
#   pwsh packaging/windows/sign.ps1 <file>...          sign them, then check them
#   pwsh packaging/windows/sign.ps1 -Check <file>...   fail unless signed as this mode signs; who
#                                                      signed them on the output and the job summary
#   pwsh packaging/windows/sign.ps1 -Mode              print the mode
#   pwsh packaging/windows/sign.ps1 -Iscc              iscc's arguments to sign Setup and Uninstall
#   pwsh packaging/windows/sign.ps1 -Trust <file>      trust the test publisher another runner made
# For the update test, test mode also signs as -Publisher other (another publisher under the test
# root) or look-alike (the test publisher's name and profile EKU, self-signed). The test publisher's
# key stays on the runner that made it; what anyone may know of it (its root's certificate, its
# profile EKUs, the certificates a runner checking its signatures must trust) is in
# $RUNNER_TEMP/earwig-test-publisher.json, which -Trust takes on another runner.
[CmdletBinding(PositionalBinding = $false)]
param(
    [switch]$Check,
    [switch]$Mode,
    [switch]$Iscc,
    [string]$Trust,
    [ValidateSet('earwig', 'other', 'look-alike')]
    [string]$Publisher = 'earwig',
    [Parameter(Position = 0, ValueFromRemainingArguments)]
    [string[]]$Files
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. "$PSScriptRoot\find.ps1"

$variables = 'ARTIFACT_SIGNING_ENDPOINT', 'ARTIFACT_SIGNING_ACCOUNT', 'ARTIFACT_SIGNING_PROFILE', 'AZURE_TENANT_ID', 'AZURE_CLIENT_ID'
$signing = if ($env:GITHUB_ACTIONS -ne 'true') { 'none' }
elseif ($env:GITHUB_REF -like 'refs/tags/v*') { 'artifact-signing' }
else { 'test' }

# A failure GitHub shows on the run, without its log.
function Fail([string]$why) {
    if ($env:GITHUB_ACTIONS -eq 'true') { Write-Host "::error title=signing::$($why -replace '%', '%25' -replace "`r", '' -replace "`n", '%0A')" }
    throw $why
}

$missing = @($variables | Where-Object { -not [Environment]::GetEnvironmentVariable($_) })
if ($signing -eq 'artifact-signing' -and $missing -and -not $Check) {
    Fail "a v* tag is signed with Artifact Signing, which needs $($missing -join ', ') set: the release environment's variables"
}
if ($Mode) { return $signing }
if ($Iscc) {
    if ($signing -ne 'none') { "/Searwig=pwsh -NoProfile -File `$q$PSCommandPath`$q `$f", '/DSignTool=earwig' }
    return
}
if ($signing -eq 'none') { return }

$timestamps = 'http://timestamp.acs.microsoft.com'
# Microsoft Identity Verification Root Certificate Authority 2020 (its SHA-256), where every
# Artifact Signing Public Trust chain ends.
$microsoftRoot = '5367F20C7ADE0E2BCA790915056D086B720C33C1FA2A2661ACF787E3292E1270'
# earwig's certificate profile in Artifact Signing: the EKU each of its certificates carries.
$earwigProfile = '1.3.6.1.4.1.311.97.320264061.530968931.474248062.405732374'
$publicTrust = '1.3.6.1.4.1.311.97.1.0'
$codeSigning = '1.3.6.1.5.5.7.3.3'
$public = "$env:RUNNER_TEMP\earwig-test-publisher.json"

# The certificate profile EKUs among $ekus: right under 1.3.6.1.4.1.311.97, not its .1 (kinds of
# profile), as the updater takes them (crates/earwig-core/src/update/mod.rs).
function Profiles([string[]]$ekus) {
    @($ekus | Where-Object { $_ -match '^1\.3\.6\.1\.4\.1\.311\.97\.(?!1(\.|$))\d' })
}

function NewCertificate([string]$subject, $issuer, [string[]]$ekus, [bool]$authority, [DateTimeOffset]$from, [DateTimeOffset]$to) {
    $key = [RSA]::Create(3072)
    $request = [CertificateRequest]::new($subject, $key, [HashAlgorithmName]::SHA256, [RSASignaturePadding]::Pkcs1)
    $request.CertificateExtensions.Add([X509BasicConstraintsExtension]::new($authority, $authority, 0, $true))
    $usage = if ($authority) { 'KeyCertSign, CrlSign' } else { 'DigitalSignature' }
    $request.CertificateExtensions.Add([X509KeyUsageExtension]::new($usage, $true))
    if ($ekus) {
        $oids = [OidCollection]::new()
        foreach ($eku in $ekus) { $null = $oids.Add([Oid]::new($eku)) }
        $request.CertificateExtensions.Add([X509EnhancedKeyUsageExtension]::new($oids, $false))
    }
    $request.CertificateExtensions.Add([X509SubjectKeyIdentifierExtension]::new($request.PublicKey, $false))
    if (-not $issuer) { return $request.CreateSelfSigned($from, $to) }
    $request.CertificateExtensions.Add([X509AuthorityKeyIdentifierExtension]::CreateFromCertificate($issuer, $true, $false))
    [RSACertificateExtensions]::CopyWithPrivateKey($request.Create($issuer, $from, $to, [RandomNumberGenerator]::GetBytes(16)), $key)
}

# Trusted by this whole machine, which only a CI runner may do with a key lying in a file.
function TrustMachineWide($certificate) {
    $store = [X509Store]::new('Root', 'LocalMachine')
    $store.Open('ReadWrite')
    $store.Add([X509Certificate2]::new($certificate.RawData))
    $store.Close()
}

# The test publisher's root, with its key, and the profile EKUs of 'earwig' and 'other', made on
# this runner's first signing; its public part in $public.
function TestRoot {
    if ($env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) {
        throw 'test signing trusts a throwaway root on the whole machine: GitHub Actions runners only'
    }
    $saved = Join-Path $env:RUNNER_TEMP 'earwig-test-signing.json'
    if (-not (Test-Path $saved)) {
        if (Test-Path $public) { throw 'this runner took the test publisher from another (-Trust): it checks signatures, it does not sign' }
        $now = [DateTimeOffset]::UtcNow
        $root = NewCertificate 'CN=earwig test root' $null @() $true $now.AddMinutes(-10) $now.AddDays(2)
        TrustMachineWide $root
        $arcs = { (1..4 | ForEach-Object { Get-Random -Minimum 2 -Maximum 1073741824 }) -join '.' }
        $state = @{
            pfx = [Convert]::ToBase64String($root.Export([X509ContentType]::Pkcs12))
            earwig = "1.3.6.1.4.1.311.97.$(& $arcs)"
            other = "1.3.6.1.4.1.311.97.$(& $arcs)"
        }
        $state | ConvertTo-Json | Set-Content $saved
        @{
            root = [Convert]::ToBase64String($root.RawData)
            earwig = $state.earwig
            other = $state.other
            trusted = @([Convert]::ToBase64String($root.RawData))
        } | ConvertTo-Json | Set-Content $public
    }
    $state = Get-Content -Raw $saved | ConvertFrom-Json
    [pscustomobject]@{
        Certificate = [X509Certificate2]::new([Convert]::FromBase64String($state.pfx), [string]$null, 'Exportable')
        Earwig = $state.earwig
        Other = $state.other
    }
}

# The test publisher as signatures are checked against it: its root's certificate and profile EKU.
function TestPublisher {
    if (-not (Test-Path $public)) { throw 'no test publisher on this runner: test signing makes one, -Trust takes one made on another runner' }
    $state = Get-Content -Raw $public | ConvertFrom-Json
    [pscustomobject]@{
        Root = [X509Certificate2]::new([Convert]::FromBase64String($state.root))
        Earwig = $state.earwig
    }
}

# A certificate for this one signing, valid for minutes (Artifact Signing's for three days): the
# timestamp keeps its signatures valid once it has expired.
function TestCertificate([string]$as) {
    $root = TestRoot
    $now = [DateTimeOffset]::UtcNow
    $subject = 'CN=earwig test publisher, O=earwig test publisher, C=US'
    switch ($as) {
        'earwig' { NewCertificate $subject $root.Certificate @($publicTrust, $codeSigning, $root.Earwig) $false $now.AddMinutes(-5) $now.AddMinutes(10) }
        'other' { NewCertificate 'CN=earwig test other, O=earwig test other, C=US' $root.Certificate @($publicTrust, $codeSigning, $root.Other) $false $now.AddMinutes(-5) $now.AddMinutes(10) }
        'look-alike' {
            $self = NewCertificate $subject $null @($publicTrust, $codeSigning, $root.Earwig) $false $now.AddMinutes(-5) $now.AddMinutes(10)
            TrustMachineWide $self
            # Trusted where its signatures are checked, too.
            $state = Get-Content -Raw $public | ConvertFrom-Json
            $state.trusted = @($state.trusted) + [Convert]::ToBase64String($self.RawData)
            $state | ConvertTo-Json | Set-Content $public
            $self
        }
    }
}

function Sign([string[]]$files) {
    $tool = SignTool
    $common = @('sign', '/fd', 'SHA256', '/tr', $timestamps, '/td', 'SHA256')
    if ($signing -eq 'test') {
        $certificate = TestCertificate $Publisher
        $password = [guid]::NewGuid().ToString()
        $pfx = Join-Path $env:RUNNER_TEMP "earwig-test-$([guid]::NewGuid()).pfx"
        [IO.File]::WriteAllBytes($pfx, $certificate.Export([X509ContentType]::Pkcs12, $password))
        try { $output = & $tool.FullName @common /f $pfx /p $password @files 2>&1 }
        finally { Remove-Item $pfx }
        $as = $certificate.Subject
    } else {
        if (-not $env:ACTIONS_ID_TOKEN_REQUEST_URL) { throw 'Artifact Signing signs in with the OIDC token of a job with permissions: id-token: write' }
        if ([version]$tool.Directory.Parent.Name -lt [version]'10.0.22621.0') { throw "the Artifact Signing dlib needs SignTool 10.0.22621 or later, not $($tool.FullName)" }
        $dlib = $env:ARTIFACT_SIGNING_DLIB
        if (-not $dlib) { throw "no Artifact Signing client: packaging/windows/tools.ps1 ArtifactSigning installs Microsoft's dlib" }
        $metadata = Join-Path $env:RUNNER_TEMP 'artifact-signing-metadata.json'
        @{
            Endpoint = $env:ARTIFACT_SIGNING_ENDPOINT
            CodeSigningAccountName = $env:ARTIFACT_SIGNING_ACCOUNT
            CertificateProfileName = $env:ARTIFACT_SIGNING_PROFILE
            CorrelationId = "$env:GITHUB_REPOSITORY $env:GITHUB_REF_NAME run $env:GITHUB_RUN_ID"
            # Only the workload identity: the OIDC token in AZURE_FEDERATED_TOKEN_FILE.
            ExcludeCredentials = @('EnvironmentCredential', 'ManagedIdentityCredential', 'SharedTokenCacheCredential',
                'VisualStudioCredential', 'VisualStudioCodeCredential', 'AzureCliCredential', 'AzurePowerShellCredential',
                'AzureDeveloperCliCredential', 'InteractiveBrowserCredential')
        } | ConvertTo-Json | Set-Content $metadata
        # A token for each signing: GitHub's last five minutes, and signing comes late in a build.
        $token = (Invoke-RestMethod -Headers @{ Authorization = "bearer $env:ACTIONS_ID_TOKEN_REQUEST_TOKEN" } `
                -Uri "$env:ACTIONS_ID_TOKEN_REQUEST_URL&audience=api://AzureADTokenExchange").value
        Write-Host "::add-mask::$token"
        $claims = $token.Split('.')[1].Replace('-', '+').Replace('_', '/')
        $claims = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($claims + '=' * ((4 - $claims.Length % 4) % 4))) | ConvertFrom-Json
        $as = "$($claims.sub) (app $env:AZURE_CLIENT_ID), profile $env:ARTIFACT_SIGNING_PROFILE of $env:ARTIFACT_SIGNING_ACCOUNT at $env:ARTIFACT_SIGNING_ENDPOINT"
        $env:AZURE_FEDERATED_TOKEN_FILE = Join-Path $env:RUNNER_TEMP "oidc-$([guid]::NewGuid())"
        Set-Content -LiteralPath $env:AZURE_FEDERATED_TOKEN_FILE -Value $token -NoNewline
        try { $output = & $tool.FullName @common /v /dlib $dlib /dmdf $metadata @files 2>&1 }
        finally { Remove-Item $env:AZURE_FEDERATED_TOKEN_FILE; Remove-Item Env:AZURE_FEDERATED_TOKEN_FILE }
    }
    Write-Host "$signing as $as`: $(($output | Select-Object -Last 3) -join ' ')"
    if ($LASTEXITCODE) {
        $why = @($output | Where-Object { "$_" -match 'error|AADSTS|Forbidden|denied|status' } | Select-Object -First 12) -join "`n"
        throw "signing $($files -join ', ') as $as failed (signtool exit $LASTEXITCODE):`n$why"
    }
}

# Why $file isn't signed as this mode signs, or nothing; how it is signed in the log and $signed
# when it is.
function Problem([string]$file) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file
    if ($signature.Status -ne 'Valid') { return "$($signature.Status): $($signature.StatusMessage)" }
    if (-not $signature.TimeStamperCertificate) { return 'no timestamp' }
    $leaf = $signature.SignerCertificate
    $profiles = Profiles @($leaf.Extensions | Where-Object { $_ -is [X509EnhancedKeyUsageExtension] } |
            ForEach-Object { $_.EnhancedKeyUsages } | ForEach-Object Value)
    $chain = [X509Chain]::new()
    $chain.ChainPolicy.RevocationMode = 'NoCheck'
    # The signing certificate may have expired since; the timestamp vouches for when it signed.
    $chain.ChainPolicy.VerificationFlags = 'IgnoreNotTimeValid'
    $bag = [X509Certificate2Collection]::new()
    try { $bag.Import($file) } catch { }
    $chain.ChainPolicy.ExtraStore.AddRange($bag)
    $null = $chain.Build($leaf)
    $root = $chain.ChainElements[$chain.ChainElements.Count - 1].Certificate
    $rootHash = $root.GetCertHashString([HashAlgorithmName]::SHA256)
    if ($signing -eq 'test') {
        $test = TestPublisher
        if ($rootHash -ne $test.Root.GetCertHashString([HashAlgorithmName]::SHA256)) { return "chains to $($root.Subject), not the test root" }
        if ($profiles -notcontains $test.Earwig) { return "certificate profile $profiles, not the test publisher's $($test.Earwig)" }
    } else {
        if ($rootHash -ne $microsoftRoot) { return "chains to $($root.Subject) ($rootHash), not Microsoft's Artifact Signing root" }
        if ($profiles -notcontains $earwigProfile) { return "$($leaf.Subject) has certificate profile $(if ($profiles) { $profiles } else { 'none' }), not earwig's $earwigProfile" }
    }
    $signer = "$($leaf.Subject), certificate profile $($profiles -join ' ')"
    $by = "root $($root.GetNameInfo('SimpleName', $false)) $($root.Thumbprint), timestamp by $($signature.TimeStamperCertificate.GetNameInfo('SimpleName', $false))"
    Write-Host "$(Split-Path -Leaf $file): $signer, $by"
    $signed.Add([pscustomobject]@{ File = (Split-Path -Leaf $file); Signer = $signer; By = $by })
}

if ($Trust) {
    if ($signing -ne 'test' -or $env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) {
        throw "-Trust trusts a throwaway root on the whole machine: test signing on GitHub Actions runners only, not $signing"
    }
    foreach ($certificate in (Get-Content -Raw $Trust | ConvertFrom-Json).trusted) {
        TrustMachineWide ([X509Certificate2]::new([Convert]::FromBase64String($certificate)))
    }
    Copy-Item $Trust $public
    return
}
if (-not $Files) { throw 'no files' }
if (-not $Check) {
    try { Sign $Files } catch { Fail "$_" }
    # Another publisher's signatures are for the update test to refuse.
    if ($Publisher -ne 'earwig') { return }
}
$signed = [Collections.Generic.List[object]]::new()
$problems = foreach ($file in $Files) {
    $problem = Problem $file
    if ($problem) { "$(Split-Path -Leaf $file): $problem" }
}
if ($problems) { throw "not signed as $signing signs:`n$($problems -join "`n")" }
if ($Check) {
    if ($env:GITHUB_STEP_SUMMARY) {
        foreach ($group in $signed | Group-Object Signer, By) {
            $names = ($group.Group.File | ForEach-Object { '`' + $_ + '`' }) -join ', '
            "${names}: signed by $($group.Group[0].Signer), $($group.Group[0].By)`n" | Add-Content $env:GITHUB_STEP_SUMMARY
        }
    }
    $signed.Signer | Select-Object -Unique
}
