# Fallback only: the app registers the helper task itself on first launch.
# Use this if the UAC prompt was declined. Run as administrator.
$exe = Join-Path $PSScriptRoot 'serein.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    throw "serein.exe not found next to this script"
}
& $exe --zapret-install-task
if ($LASTEXITCODE -ne 0) { throw "task registration failed with $LASTEXITCODE" }
Write-Host "SereinZapret task registered."
