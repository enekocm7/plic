<#
.SYNOPSIS
Fills in the Scoop manifest template for a release.

.DESCRIPTION
Reads the template under packaging/scoop, replaces {{VERSION}},
{{RELEASE_DATE}} and {{ZIP_SHA256}}, and writes the result to OutDir/scoop. The
release workflow runs it with the hashes of the files it just built.

.EXAMPLE
pwsh packaging/render-manifests.ps1 -Version 0.9.0 `
    -Zip target/dist/plic-0.9.0-windows-x64.zip -OutDir target/dist/manifests
#>
param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Zip,
    [Parameter(Mandatory)] [string] $OutDir,
    [string] $ReleaseDate = (Get-Date -Format 'yyyy-MM-dd')
)

$ErrorActionPreference = 'Stop'

$values = @{
    '{{VERSION}}'          = $Version
    '{{RELEASE_DATE}}'     = $ReleaseDate
    '{{ZIP_SHA256}}'       = (Get-FileHash $Zip -Algorithm SHA256).Hash.ToLower()
}

function Render([string] $Template, [string] $Destination) {
    $text = Get-Content -Raw $Template
    foreach ($key in $values.Keys) {
        $text = $text.Replace($key, $values[$key])
    }
    if ($text -match '\{\{[A-Z_]+\}\}') {
        throw "$Template still has a placeholder: $($Matches[0])"
    }
    New-Item -ItemType Directory -Force (Split-Path $Destination) | Out-Null
    # UTF-8 without a byte order mark, as Scoop expects.
    [System.IO.File]::WriteAllText($Destination, $text, [System.Text.UTF8Encoding]::new($false))
}

$here = $PSScriptRoot
Render (Join-Path $here 'scoop/plic.json') (Join-Path $OutDir 'scoop/plic.json')

# The Scoop manifest must stay valid JSON once filled in.
Get-Content -Raw (Join-Path $OutDir 'scoop/plic.json') | ConvertFrom-Json | Out-Null
Write-Output "Manifests for $Version written to $OutDir"
