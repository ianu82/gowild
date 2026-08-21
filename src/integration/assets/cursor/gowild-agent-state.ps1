# managed by gowild; reinstalling the integration replaces this file.
# GOWILD_INTEGRATION_ID=cursor
# GOWILD_INTEGRATION_VERSION=1

param([string]$Action = "")

function Exit-Hook {
    Write-Output "{}"
    exit 0
}

if ($Action -ne "session") { Exit-Hook }
if ($env:GOWILD_ENV -ne "1") { Exit-Hook }
if ([string]::IsNullOrWhiteSpace($env:GOWILD_PANE_ID)) { Exit-Hook }

$inputText = [Console]::In.ReadToEnd()
$jsonStart = $inputText.IndexOf("{")
if ($jsonStart -gt 0) {
    $inputText = $inputText.Substring($jsonStart)
}
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    Exit-Hook
}

if ($null -eq $payload) { Exit-Hook }
$event = if ($payload.hook_event_name -is [string]) { $payload.hook_event_name } else { $payload.hookEventName }
if (-not [string]::IsNullOrWhiteSpace($event) -and $event -ne "sessionStart") { Exit-Hook }

$sessionId = $null
foreach ($name in @("session_id", "sessionId", "conversation_id", "conversationId")) {
    $value = $payload.$name
    if ($value -is [string] -and -not [string]::IsNullOrWhiteSpace($value)) {
        $sessionId = $value
        break
    }
}
if ([string]::IsNullOrWhiteSpace($sessionId)) { Exit-Hook }

$seq = [DateTime]::UtcNow.Ticks
$gowild = if ([string]::IsNullOrWhiteSpace($env:GOWILD_BIN_PATH)) { "gowild" } else { $env:GOWILD_BIN_PATH }
try {
    & $gowild pane report-agent-session $env:GOWILD_PANE_ID --source gowild:cursor --agent cursor --seq $seq --agent-session-id $sessionId 2>$null | Out-Null
} catch {
}

Exit-Hook
