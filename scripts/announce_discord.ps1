# Announces a release in the Discord server (#releases) through a webhook.
#   $env:DISCORD_RELEASE_WEBHOOK  webhook URL (repository secret)
#   $env:DISCORD_AVISOS_ROLE      id of the role pinged for new versions (optional)
# Text: the highlights (bold paragraphs) of release-notes/<tag>.md, in both languages.
param([Parameter(Mandatory = $true)][string]$Tag)
$ErrorActionPreference = "Stop"
if (-not $env:DISCORD_RELEASE_WEBHOOK) { Write-Host "announce: no webhook configured"; exit 0 }
$root = Split-Path $PSScriptRoot -Parent
$notes = Join-Path $root "release-notes\$Tag.md"
$repo = "https://github.com/ragnarcb/obs-dynamic-delay"
$download = "$repo/releases/latest/download/Dynamic-Delay-Setup.exe"

function Highlights([string]$section) {
    # paragraphs that start with a bold title, without the download line
    ($section -split "(\r?\n){2,}") | Where-Object { $_ -match '^\*\*' -and $_ -notmatch 'Dynamic-Delay-Setup\.exe' } |
        ForEach-Object { $_.Trim() } | Select-Object -First 3
}
$en, $pt = @(), @()
if (Test-Path $notes) {
    $text = Get-Content $notes -Raw -Encoding utf8
    $parts = $text -split '(?m)^## '
    $enPart = $parts | Where-Object { $_ -like 'English*' } | Select-Object -First 1
    $ptPart = $parts | Where-Object { $_ -like 'Português*' } | Select-Object -First 1
    if ($ptPart) { $pt = Highlights $ptPart }
    if ($enPart) { $en = Highlights $enPart }
}
$desc = ""
if ($pt) { $desc += "**🇧🇷**`n" + ($pt -join "`n`n") + "`n`n" }
if ($en) { $desc += "**🇺🇸**`n" + ($en -join "`n`n") + "`n`n" }
if ($desc.Length -gt 3500) { $desc = $desc.Substring(0, 3500) + "…`n`n" }
$desc += "**⬇️ Download:** $download"

$ping = if ($env:DISCORD_AVISOS_ROLE) { "<@&$($env:DISCORD_AVISOS_ROLE)> " } else { "" }
$body = @{
    username         = "Dynamic Delay"
    content          = "$ping**Dynamic Delay $Tag** · nova versão / new version"
    allowed_mentions = @{ roles = @($env:DISCORD_AVISOS_ROLE | Where-Object { $_ }) }
    embeds           = @(@{
        title       = "Dynamic Delay $Tag"
        url         = "$repo/releases/tag/$Tag"
        description = $desc
        color       = 0xe5484d
        footer      = @{ text = "github.com/ragnarcb/obs-dynamic-delay" }
    })
} | ConvertTo-Json -Depth 6
Invoke-RestMethod -Method Post -Uri $env:DISCORD_RELEASE_WEBHOOK -ContentType "application/json; charset=utf-8" -Body ([Text.Encoding]::UTF8.GetBytes($body)) | Out-Null
Write-Host "announce: $Tag posted to Discord"
