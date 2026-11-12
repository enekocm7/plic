<#
.SYNOPSIS
Fills in the winget and Scoop manifest templates for a release.

.DESCRIPTION
Reads the templates under packaging/winget and packaging/scoop, replaces
{{VERSION}}, {{RELEASE_DATE}}, {{INSTALLER_SHA256}} and {{ZIP_SHA256}}, and
writes the results to OutDir/winget/<id>/<version> and OutDir/scoop. The
release workflow runs it with the hashes of the files it just built.

.EXAMPLE
pwsh packaging/render-manifests.ps1 -Version 0.9.0 -Installer target/dist/plic-0.9.0-setup.exe `
    -Zip target/dist/plic-0.9.0-windows-x64.zip -OutDir target/dist/manifests
#>
param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Installer,
    [Parameter(Mandatory)] [string] $Zip,
    [Parameter(Mandatory)] [string] $OutDir,
    [string] $ReleaseDate = (Get-Date -Format 'yyyy-MM-dd')
)

$ErrorActionPreference = 'Stop'

$values = @{
    '{{VERSION}}'          = $Version
    '{{RELEASE_DATE}}'     = $ReleaseDate
    '{{INSTALLER_SHA256}}' = (Get-FileHash $Installer -Algorithm SHA256).Hash.ToUpper()
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
    # UTF-8 without a byte order mark, as winget and Scoop expect.
    [System.IO.File]::WriteAllText($Destination, $text, [System.Text.UTF8Encoding]::new($false))
}

$here = $PSScriptRoot
$winget = Join-Path $OutDir "winget/enekocm7.plic/$Version"
Get-ChildItem (Join-Path $here 'winget') -Filter '*.yaml' | ForEach-Object {
    Render $_.FullName (Join-Path $winget $_.Name)
}
Render (Join-Path $here 'scoop/plic.json') (Join-Path $OutDir 'scoop/plic.json')

# The Scoop manifest must stay valid JSON once filled in.
Get-Content -Raw (Join-Path $OutDir 'scoop/plic.json') | ConvertFrom-Json | Out-Null
Write-Output "Manifests for $Version written to $OutDir"
