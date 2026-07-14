<#
.SYNOPSIS
  Package UT4 editor-target plugin binaries into flat-root zips for the launcher's
  signed `editor_plugins` manifest channel (editor-install management, Phase 1).

.DESCRIPTION
  For each plugin, gathers from the build tree:
    <plugin>.uplugin
    Binaries\Win64\UE4Editor-*.dll
    Binaries\Win64\UE4Editor.modules
    Content\**                 (only if the plugin ships its own editor content)
  into a flat staging dir, then zips it FLAT-ROOT (no wrapper dir). The game /
  server DLL variants (UE4-*, UE4Server-*) and .pdb symbols are deliberately
  excluded — the editor never loads them, and shipping them is what created stale
  over-copied DLLs in editor trees.

  Emits, per plugin, the sha256 + size + engine BuildId/Changelist and a ready
  `editor_plugins` JSON block to paste into the signed manifest. This script
  ships NOTHING itself: upload the zips to ONE GitHub release
  (editor-plugins-latest), then edit + YubiKey-sign the manifest per
  docs\EDITOR-INSTALLS-DESIGN.md (and the launcher release runbook). Editor
  plugins have no server-gate, so they can publish independently of a game roll.

.PARAMETER BuildTree
  The UT4 build tree PROJECT dir (the one holding Plugins\). Default:
  C:\UnrealTournament\UnrealTournament

.PARAMETER OutDir
  Where the zips are written. Default: $env:TEMP\ncp-editor-plugins

.PARAMETER Plugins
  Plugin DIR names to package. Default: NetcodePlus, UTVehicles, LiandriMapForge.
  These are dir names; the module name inside can differ (LiandriMapForge's module
  is MapForgeBridge) — the UE4Editor-*.dll glob handles that.

.EXAMPLE
  pwsh tools\pack-editor-plugins.ps1
  pwsh tools\pack-editor-plugins.ps1 -Plugins NetcodePlus
#>
[CmdletBinding()]
param(
    [string]$BuildTree = "C:\UnrealTournament\UnrealTournament",
    [string]$OutDir = (Join-Path $env:TEMP "ncp-editor-plugins"),
    [string[]]$Plugins = @("NetcodePlus", "UTVehicles", "LiandriMapForge")
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem

if (-not (Test-Path (Join-Path $BuildTree "Plugins"))) {
    throw "BuildTree '$BuildTree' has no Plugins\ folder — is it the UT4 project dir?"
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$results = @()

foreach ($plugin in $Plugins) {
    $src = Join-Path $BuildTree "Plugins\$plugin"
    if (-not (Test-Path $src)) { Write-Warning "skip ${plugin}: no $src"; continue }

    $win64 = Join-Path $src "Binaries\Win64"
    $editorDlls = @()
    if (Test-Path $win64) {
        $editorDlls = @(Get-ChildItem $win64 -Filter "UE4Editor-*.dll" -File -ErrorAction SilentlyContinue)
    }
    if ($editorDlls.Count -eq 0) {
        Write-Warning "skip ${plugin}: no UE4Editor-*.dll in $win64 (not built for the editor target?)"
        continue
    }

    # Fresh flat staging dir.
    $stage = Join-Path $OutDir ".stage-$plugin"
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null

    # <plugin>.uplugin at the flat root.
    $uplugin = Join-Path $src "$plugin.uplugin"
    if (Test-Path $uplugin) { Copy-Item $uplugin (Join-Path $stage "$plugin.uplugin") }

    # Binaries\Win64: editor DLLs + the .modules only (no UE4-*/UE4Server-*, no PDBs).
    $stageWin64 = Join-Path $stage "Binaries\Win64"
    New-Item -ItemType Directory -Force -Path $stageWin64 | Out-Null
    foreach ($dll in $editorDlls) { Copy-Item $dll.FullName $stageWin64 }
    $modules = Join-Path $win64 "UE4Editor.modules"
    if (Test-Path $modules) { Copy-Item $modules $stageWin64 }

    # The plugin's own Content\ (editor content), if any — distinct from project Content.
    $content = Join-Path $src "Content"
    if (Test-Path $content) { Copy-Item $content (Join-Path $stage "Content") -Recurse }

    # Engine stamp from the .modules (for the manifest entry's compat warning).
    $buildId = $null; $changelist = $null
    if (Test-Path $modules) {
        try {
            $m = Get-Content $modules -Raw | ConvertFrom-Json
            $buildId = $m.BuildId
            $changelist = $m.Changelist
        } catch { Write-Warning "${plugin}: couldn't parse UE4Editor.modules ($_)" }
    }

    # Zip flat-root: the final $false = do NOT include the base dir name, so the
    # .uplugin + Binaries\ land at the zip root (matches the game-plugin recipe).
    $out = Join-Path $OutDir "$plugin-editor.zip"
    if (Test-Path $out) { Remove-Item $out -Force }
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $stage, $out, [System.IO.Compression.CompressionLevel]::Optimal, $false)
    Remove-Item $stage -Recurse -Force

    $sha = (Get-FileHash $out -Algorithm SHA256).Hash.ToLower()
    $size = (Get-Item $out).Length
    $results += [PSCustomObject]@{
        Plugin           = $plugin
        Zip              = $out
        Sha256           = $sha
        SizeBytes        = $size
        EngineBuildId    = $buildId
        EngineChangelist = $changelist
    }
    Write-Output ("packed {0,-16} {1,10:N0} B  sha256 {2}  (engine {3} / CL {4})" -f `
            $plugin, $size, $sha, $buildId, $changelist)
}

if ($results.Count -eq 0) { throw "no plugins packaged — nothing to emit" }

# Build a ready `editor_plugins` block. `version` is 0 as a placeholder — set each
# to the plugin's real build number before signing (there is no reliable version
# in the .uplugin to read it from).
$map = [ordered]@{}
foreach ($r in $results) {
    $entry = [ordered]@{
        version    = 0
        url        = "https://github.com/jmortley/netcodeplus-launcher/releases/download/editor-plugins-latest/$($r.Plugin)-editor.zip"
        sha256     = $r.Sha256
        size_bytes = $r.SizeBytes
    }
    if ($r.EngineBuildId) { $entry["engine_build_id"] = $r.EngineBuildId }
    if ($null -ne $r.EngineChangelist) { $entry["engine_changelist"] = [int64]$r.EngineChangelist }
    $map[$r.Plugin] = $entry
}

Write-Output ""
Write-Output "=== paste into the signed manifest as `"editor_plugins`" (set each version!) ==="
Write-Output (ConvertTo-Json -InputObject $map -Depth 5)
Write-Output ""
Write-Output "Next: gh release upload editor-plugins-latest $OutDir\*-editor.zip --clobber ; then edit + sign the manifest."
