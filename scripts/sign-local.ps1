<#
.SYNOPSIS
    Local fallback: Authenticode-sign the launcher exe with Azure Artifact Signing,
    for use when the GitHub Actions release workflow (.github/workflows/release.yml)
    is unavailable. CI is the primary path; this is the break-glass alternative.

.DESCRIPTION
    Mirrors what the CI signing step does, but on this box using `az login`
    (DefaultAzureCredential -> AzureCliCredential) instead of OIDC.

    Order matters and matches the ship ritual:
        build (--no-bundle) -> rcedit --set-icon -> SIGN (this script) -> hash-pin.
    Run this on the ALREADY icon-applied, final-named exe. Signing mutates the bytes,
    so compute the manifest sha256/size AFTER this script (it prints them for you).

    Prereqs (one-time): install the Artifact Signing client tools, which bundle a
    compatible signtool.exe + the Azure.CodeSigning.Dlib + .NET 8 runtime:
        winget install -e --id Microsoft.Azure.ArtifactSigningClientTools
    and the Azure CLI (`winget install -e --id Microsoft.AzureCLI`). Then `az login`
    with an account that holds the "Artifact Signing Certificate Profile Signer" role
    on the certificate profile.

.PARAMETER ExePath
    Path to the staged, icon-applied, final-named exe (UT4-Community-Launcher-X.Y.Z.exe).

.PARAMETER Endpoint
    Region endpoint, e.g. https://eus.codesigning.azure.net  (East US).
    Defaults to $env:AZURE_SIGNING_ENDPOINT.

.PARAMETER AccountName
    Artifact Signing account name. Defaults to $env:AZURE_SIGNING_ACCOUNT.

.PARAMETER ProfileName
    Certificate profile name. Defaults to $env:AZURE_SIGNING_PROFILE.

.EXAMPLE
    pwsh scripts/sign-local.ps1 -ExePath "$env:TEMP\ncp-rel\UT4-Community-Launcher-1.4.2.exe" `
        -Endpoint https://eus.codesigning.azure.net -AccountName ut4launcher -ProfileName ut4launcher-public
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $ExePath,
    [string] $Endpoint    = $env:AZURE_SIGNING_ENDPOINT,
    [string] $AccountName = $env:AZURE_SIGNING_ACCOUNT,
    [string] $ProfileName = $env:AZURE_SIGNING_PROFILE,
    [string] $SignToolPath,        # auto-discovered if omitted
    [string] $DlibPath             # auto-discovered if omitted
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $ExePath)) { throw "Exe not found: $ExePath" }
foreach ($p in @{ Endpoint = $Endpoint; AccountName = $AccountName; ProfileName = $ProfileName }.GetEnumerator()) {
    if ([string]::IsNullOrWhiteSpace($p.Value)) { throw "Missing required value: $($p.Key) (pass -$($p.Key) or set the matching AZURE_SIGNING_* env var)" }
}

function Find-First([string[]] $candidates) {
    foreach ($c in $candidates) {
        $hit = Get-ChildItem -Path $c -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    return $null
}

if (-not $SignToolPath) {
    $SignToolPath = Find-First @(
        "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe"
    )
    if (-not $SignToolPath) { throw "signtool.exe not found under Windows Kits 10. Install the Windows 10/11 SDK (>= 10.0.2261.755)." }
}
if (-not $DlibPath) {
    $DlibPath = Find-First @(
        "${env:ProgramFiles}\Microsoft\Artifact Signing Client Tools\**\x64\Azure.CodeSigning.Dlib.dll",
        "${env:LOCALAPPDATA}\**\Azure.CodeSigning.Dlib.dll",
        "$PSScriptRoot\..\**\Azure.CodeSigning.Dlib.dll"
    )
    if (-not $DlibPath) { throw "Azure.CodeSigning.Dlib.dll not found. Install: winget install -e --id Microsoft.Azure.ArtifactSigningClientTools" }
}

Write-Host "signtool : $SignToolPath"
Write-Host "dlib     : $DlibPath"
Write-Host "endpoint : $Endpoint"

# Ensure an Azure session exists (AzureCliCredential is what the dlib will use).
& az account show 1>$null 2>$null
if ($LASTEXITCODE -ne 0) { throw "Not logged in to Azure. Run: az login   (account must hold 'Artifact Signing Certificate Profile Signer' on the profile)" }

# metadata.json for the dlib — exclude every credential except AzureCli so it doesn't
# stall probing managed identity / interactive on a workstation.
$meta = [ordered]@{
    Endpoint               = $Endpoint
    CodeSigningAccountName = $AccountName
    CertificateProfileName = $ProfileName
    ExcludeCredentials     = @(
        "ManagedIdentityCredential","WorkloadIdentityCredential","SharedTokenCacheCredential",
        "VisualStudioCredential","VisualStudioCodeCredential","AzurePowerShellCredential",
        "AzureDeveloperCliCredential","InteractiveBrowserCredential","EnvironmentCredential"
    )
}
$metaPath = Join-Path $env:TEMP "ncp-signing-metadata.json"
($meta | ConvertTo-Json) | Out-File -FilePath $metaPath -Encoding ascii
Write-Host "metadata : $metaPath"

# Sign. Timestamping is mandatory — Artifact Signing certs have a 3-day validity; the
# RFC3161 timestamp is what keeps the signature valid after the cert rolls.
& $SignToolPath sign /v /debug /fd SHA256 `
    /tr "http://timestamp.acs.microsoft.com" /td SHA256 `
    /dlib "$DlibPath" /dmdf "$metaPath" `
    "$ExePath"
if ($LASTEXITCODE -ne 0) { throw "signtool sign failed ($LASTEXITCODE)" }

# Verify.
$sig = Get-AuthenticodeSignature $ExePath
Write-Host "Status      : $($sig.Status)"
Write-Host "Signer      : $($sig.SignerCertificate.Subject)"
Write-Host "TimeStamper : $($sig.TimeStamperCertificate.Subject)"
if ($sig.Status -ne 'Valid') { throw "Authenticode signature invalid: $($sig.Status) - $($sig.StatusMessage)" }

# Hash-pin — compute AFTER signing (signing mutated the bytes). Paste into the manifest.
$sha  = (Get-FileHash $ExePath -Algorithm SHA256).Hash.ToLower()
$size = (Get-Item $ExePath).Length
Write-Host ""
Write-Host "================ MANIFEST HASH-PIN (post-sign) ================" -ForegroundColor Green
Write-Host "  `"sha256`": `"$sha`","
Write-Host "  `"size_bytes`": $size"
Write-Host "==============================================================" -ForegroundColor Green

# --- Certum fallback (no Azure): if Azure is the thing that's down, sign with a
#     Certum Open Source cert on the local cryptographic token instead:
#       & $SignToolPath sign /v /fd SHA256 /tr http://time.certum.pl /td SHA256 /a "$ExePath"
#     (/a auto-selects the token cert; plug in the token + enter the PIN when prompted.)
