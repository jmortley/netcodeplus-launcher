<#
.SYNOPSIS
  Authoring cross-check + manifest-entry generator for a NetcodePlus content pak.

.DESCRIPTION
  The signed launcher manifest pins each pak by SHA-256. UTCC reports MD5. This
  script reconciles the two at authoring time: it computes the local cooked
  pak's MD5 + SHA-256 + size, fetches UTCC's catalog entry for the same content
  id, and asserts the local MD5 == UTCC's checkSum and the sizes match -- i.e.
  "the pak I cooked is byte-for-byte the pak UTCC serves". Only then does it emit
  a ready-to-paste ManifestPak JSON block (pinned by the locally-computed
  SHA-256, with the UTCC http redirect URL).

  Pass -PakPath to verify YOUR local cooked pak (the production path -- the
  SHA-256 then certifies your bytes). Omit it to download UTCC's copy to a temp
  file and hash that instead (handy for a quick dogfood manifest).

.PARAMETER Id
  UTCC content id (the number in the page slug, e.g. 1996 for 1996-ElimPlusNC).

.PARAMETER PakId
  Stable manifest key for this pak (e.g. "elimplus"). Becomes the channel.paks
  map key. Defaults to the lowercased package name minus -windowsnoeditor.

.PARAMETER PakPath
  Path to the local cooked .pak. If omitted, downloads UTCC's current copy.

.PARAMETER Version
  Explicit manifest semver (MAJOR.MINOR.PATCH, e.g. "1.8.1"). The launcher parses
  pak.version as STRICT semver, so this is REQUIRED whenever UTCC's version name
  is not cleanly numeric -- e.g. "1.8a"/"v3.27a", which cannot auto-convert. A bad
  value breaks the WHOLE manifest (every client hangs on "Checking for updates").

.PARAMETER LoadOrder
  Engine load order for the manifest entry (higher overrides lower). Default 100.

.PARAMETER Required
  Whether the launcher must refuse to launch without this pak. Default $false.

.EXAMPLE
  .\pak-authoring.ps1 -Id 1996 -PakId elimplus -PakPath "C:\path\ElimPlusMutator-WindowsNoEditor.pak"

.EXAMPLE
  # Letter-suffixed UTCC version (e.g. "1.8a") needs an explicit semver:
  .\pak-authoring.ps1 -Id 2036 -PakId wipeout -Version 1.8.1 -PakPath "C:\path\WipeoutMutator-WindowsNoEditor.pak"
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][int]$Id,
  [string]$PakId,
  [string]$PakPath,
  [string]$Version,
  [int]$LoadOrder = 100,
  [bool]$Required = $false
)

$ErrorActionPreference = 'Stop'

# Map a UTCC version name (e.g. "v1.327", "3.27", "12") to a STRICT semver
# (major.minor.patch, all-numeric) for the manifest version field -- display/
# record only, the update trigger is hash-based. The launcher parses this as a
# real semver::Version, so a non-numeric part makes the ENTIRE manifest fail to
# deserialize (every client hangs on "Checking for updates"). We REFUSE to emit
# an invalid value and tell the caller to pass -Version -- we do NOT guess what a
# revision letter (the "a" in "1.8a") is meant to be.
function ConvertTo-Semver([string]$name) {
  $v = $name.Trim().TrimStart('v', 'V')
  $parts = $v.Split('.')
  while ($parts.Count -lt 3) { $parts += '0' }
  if ($parts.Count -gt 3) { $parts = $parts[0..2] }
  $semver = ($parts -join '.')
  if ($semver -notmatch '^\d+\.\d+\.\d+$') {
    throw ("UTCC version '$name' is not a clean numeric version (auto-converted to " +
      "'$semver', which is NOT valid semver). The launcher parses pak.version as " +
      "STRICT semver, so emitting this would break the whole manifest. Re-run with " +
      "an explicit -Version <MAJOR.MINOR.PATCH> -- e.g. for a revision like '1.8a' " +
      "pass -Version 1.8.1.")
  }
  return $semver
}

Write-Host "Fetching UTCC catalog for content id $Id ..." -ForegroundColor Cyan
$data = Invoke-RestMethod -Uri "https://utcustomcontent.com/content/$Id/data" -Headers @{ Accept = 'application/json' }

$latestId = $data.content.latestVersion.id
$version = $data.versions | Where-Object { $_.id -eq $latestId } | Select-Object -First 1
if (-not $version) { throw "Could not find latest version ($latestId) in the UTCC response for id $Id." }
$ini = $version.iniConfig
if (-not $ini -or -not $ini.checkSum) { throw "Latest UTCC version $latestId has no iniConfig.checkSum." }

$utccMd5 = $ini.checkSum.ToLower()
$utccPkg = $ini.packageName
$utccSize = [int64]$version.pakFile.size
$utccUrl = $ini.packageUrl
$utccVer = $version.name

Write-Host "  UTCC: package=$utccPkg version=$utccVer size=$utccSize md5=$utccMd5"

# Resolve the bytes to hash: the local cooked pak (production), else download
# UTCC's copy (dogfood convenience).
$cleanup = $false
if (-not $PakPath) {
  $tmp = Join-Path $env:TEMP "pak-author-$Id.pak"
  Write-Host "No -PakPath given; downloading UTCC's copy to $tmp ..." -ForegroundColor Yellow
  Invoke-WebRequest -Uri "https://$utccUrl" -OutFile $tmp
  $PakPath = $tmp
  $cleanup = $true
}
if (-not (Test-Path $PakPath)) { throw "Pak file not found: $PakPath" }

$localMd5 = (Get-FileHash -Path $PakPath -Algorithm MD5).Hash.ToLower()
$localSha = (Get-FileHash -Path $PakPath -Algorithm SHA256).Hash.ToLower()
$localSize = (Get-Item $PakPath).Length
if ($cleanup) { Remove-Item $PakPath -ErrorAction SilentlyContinue }

Write-Host "  Local: size=$localSize md5=$localMd5"
Write-Host "  Local sha256=$localSha"

# Cross-check: does my pak == what UTCC serves?
$ok = $true
if ($localMd5 -ne $utccMd5) { Write-Host "MD5 MISMATCH: local $localMd5 != UTCC $utccMd5" -ForegroundColor Red; $ok = $false }
if ($localSize -ne $utccSize) { Write-Host "SIZE MISMATCH: local $localSize != UTCC $utccSize" -ForegroundColor Red; $ok = $false }
if (-not $ok) { throw "Cross-check FAILED: do NOT add this pak until the local pak matches UTCC." }
Write-Host "Cross-check PASSED (md5 + size match UTCC)." -ForegroundColor Green

if (-not $PakId) {
  $PakId = ($utccPkg -replace '(?i)-windowsnoeditor$', '').ToLower()
}
# Honour an explicit -Version (required for letter-suffixed UTCC names); else
# derive a strict semver from the UTCC version name (throws if not numeric).
if ($Version) {
  if ($Version -notmatch '^\d+\.\d+\.\d+([-+].+)?$') {
    throw "-Version '$Version' is not valid semver. Use MAJOR.MINOR.PATCH (e.g. 1.8.1)."
  }
  $semver = $Version
}
else {
  $semver = ConvertTo-Semver $utccVer
}
$pakFilename = "$utccPkg.pak"

# Emit the ManifestPak block, keyed by $PakId, to paste into channels.<ch>.paks.
$entry = [ordered]@{
  version      = $semver
  pak_filename = $pakFilename
  url          = "http://$utccUrl"
  sha256       = $localSha
  size_bytes   = $localSize
  load_order   = $LoadOrder
  required     = $Required
}
$json = ($entry | ConvertTo-Json -Depth 4)
Write-Host ""
Write-Host "Manifest entry for '$PakId':" -ForegroundColor Cyan
Write-Output ('"' + $PakId + '": ' + $json)
