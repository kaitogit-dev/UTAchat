$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $root

$env:DATABASE_URL = "postgres://utachat:utachat-local-only@127.0.0.1:5436/utachat"
$env:UTACHAT_LISTEN_ADDR = "127.0.0.1:4800"
$env:UTACHAT_API_URL = "http://127.0.0.1:4800"

& docker compose up -d --wait
if ($LASTEXITCODE -ne 0) { throw "PostgreSQL did not start." }

& cargo build --release -p utachat-server -p utachat
if ($LASTEXITCODE -ne 0) { throw "The Rust build failed." }

$server = Start-Process -FilePath (Join-Path $root "target\release\utachat-server.exe") -PassThru -NoNewWindow
try {
    Start-Sleep -Seconds 2
    if ($server.HasExited) { throw "The backend exited before UTAchat opened." }
    & (Join-Path $root "target\release\utachat.exe")
}
finally {
    if (!$server.HasExited) { Stop-Process -Id $server.Id -ErrorAction SilentlyContinue }
}
