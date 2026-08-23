# installed by gowild
# managed by gowild; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# GOWILD_INTEGRATION_ID=qodercli
# GOWILD_INTEGRATION_VERSION=3

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:GOWILD_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:GOWILD_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

if ($null -eq $payload -or [string]::IsNullOrWhiteSpace($payload.session_id)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$gowild = if ([string]::IsNullOrWhiteSpace($env:GOWILD_BIN_PATH)) { "gowild" } else { $env:GOWILD_BIN_PATH }
try {
    & $gowild pane report-agent-session $env:GOWILD_PANE_ID --source gowild:qodercli --agent qodercli --agent-session-id $payload.session_id --seq $seq 2>$null | Out-Null
} catch {
}
