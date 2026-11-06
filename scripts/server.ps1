$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $root

if (!$env:DATABASE_URL) { throw "Set DATABASE_URL before starting the server." }
if (!$env:UTACHAT_LISTEN_ADDR) { $env:UTACHAT_LISTEN_ADDR = "127.0.0.1:4800" }

& cargo run --release -p utachat-server
if ($LASTEXITCODE -ne 0) { throw "UTAchat server exited with an error." }
