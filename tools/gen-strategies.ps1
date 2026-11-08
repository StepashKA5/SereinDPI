# Converts Flowseal/zapret-discord-youtube "general*.bat" strategies into a
# portable JSON catalog (paths replaced by {bin} / {lists} placeholders).
param(
    [string]$ZapretDir = 'C:\zapret-discord-youtube-1.10.4',
    [string]$OutFile   = (Join-Path $PSScriptRoot '..\strategies.json')
)

$ErrorActionPreference = 'Stop'

function Get-WinwsArgs([string]$Path) {
    $raw = Get-Content -LiteralPath $Path -Raw
    $lines = $raw -split "`r?`n"
    $start = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match 'winws\.exe') { $start = $i; break }
    }
    if ($start -lt 0) { return $null }

    $buffer = ''
    for ($i = $start; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($i -eq $start) {
            $idx = $line.IndexOf('winws.exe')
            $line = $line.Substring($idx + 'winws.exe'.Length)
        }
        $continues = $line.TrimEnd() -match '\^$'
        $line = $line.TrimEnd() -replace '\^\s*$', ''
        $buffer += ' ' + $line
        if (-not $continues) { break }
    }

    $buffer = $buffer -replace '%BIN%',   '{bin}'
    $buffer = $buffer -replace '%LISTS%', '{lists}'
    $buffer = $buffer -replace '%GameFilterTCP%', '{gamefilter_tcp}'
    $buffer = $buffer -replace '%GameFilterUDP%', '{gamefilter_udp}'
    # Drop shell quoting: {bin}/{lists} placeholders never contain spaces or commas.
    $buffer = $buffer -replace '"', ''

    $args = [System.Collections.Generic.List[string]]::new()
    foreach ($token in ($buffer -split '\s+')) {
        if ([string]::IsNullOrWhiteSpace($token)) { continue }
        # `--filter-udp=12` artifacts come from empty game-filter variables.
        $args.Add($token.Trim())
    }
    return ,$args.ToArray()
}

$strategies = @()
Get-ChildItem -LiteralPath $ZapretDir -Filter 'general*.bat' | Sort-Object Name | ForEach-Object {
    $argsList = Get-WinwsArgs $_.FullName
    if (-not $argsList) { Write-Warning "no winws args in $($_.Name)"; return }
    $id = ($_.BaseName -replace '^general\s*', '' -replace '[()]', '' -replace '\s+', '-' ).Trim('-').ToLowerInvariant()
    if ([string]::IsNullOrEmpty($id)) { $id = 'general' }
    $strategies += [ordered]@{
        id   = $id
        name = $_.BaseName
        args = $argsList
    }
}

$catalog = [ordered]@{
    version    = 1
    source     = 'Flowseal/zapret-discord-youtube'
    strategies = $strategies
}

$json = $catalog | ConvertTo-Json -Depth 6
# Write UTF-8 without a BOM: the JSON is embedded into the Rust binary and both
# serde_json and Fluent reject a leading BOM.
[System.IO.File]::WriteAllText(
    [System.IO.Path]::GetFullPath($OutFile),
    $json,
    (New-Object System.Text.UTF8Encoding($false))
)
Write-Host ("Wrote {0} strategies to {1}" -f $strategies.Count, (Resolve-Path $OutFile))
