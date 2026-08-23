param(
    [Parameter(Mandatory = $true)]
    [string] $ExePath,

    [string] $Session = "ci-windows-$([guid]::NewGuid().ToString('N'))"
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([string] $Command, [string[]] $Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $Command $($Arguments -join ' ')"
    }
}

$exe = (Resolve-Path $ExePath).Path
$fakeDir = Join-Path ([System.IO.Path]::GetTempPath()) "gowild-fake-conpty-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force $fakeDir | Out-Null

$fakeSource = Join-Path $fakeDir "fake_conpty.rs"
$fakeDll = Join-Path $fakeDir "conpty.dll"
@'
#![allow(non_snake_case)]

use std::ffi::c_void;

#[repr(C)]
pub struct COORD {
    pub X: i16,
    pub Y: i16,
}

type HANDLE = *mut c_void;
type HRESULT = i32;

#[no_mangle]
pub extern "system" fn CreatePseudoConsole(
    _size: COORD,
    _h_input: HANDLE,
    _h_output: HANDLE,
    _flags: u32,
    _hpc: *mut HANDLE,
) -> HRESULT {
    -2147467259
}

#[no_mangle]
pub extern "system" fn ResizePseudoConsole(_hpc: HANDLE, _size: COORD) -> HRESULT {
    -2147467259
}

#[no_mangle]
pub extern "system" fn ClosePseudoConsole(_hpc: HANDLE) {}
'@ | Set-Content -NoNewline -Encoding utf8 $fakeSource

Invoke-Checked rustc @("--crate-type", "cdylib", "--edition", "2021", $fakeSource, "-o", $fakeDll)

$oldPath = $env:PATH
$oldSession = $env:GOWILD_SESSION
$oldSocket = $env:GOWILD_SOCKET_PATH
$oldClientSocket = $env:GOWILD_CLIENT_SOCKET_PATH
$env:PATH = "$fakeDir;$oldPath"
$env:GOWILD_SESSION = $Session
Remove-Item Env:GOWILD_SOCKET_PATH, Env:GOWILD_CLIENT_SOCKET_PATH -ErrorAction SilentlyContinue

$server = $null
try {
    Invoke-Checked $exe @("--version")
    & $exe --default-config | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $exe --default-config"
    }

    $server = Start-Process -FilePath $exe -ArgumentList "server" -PassThru -WindowStyle Hidden
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 250
        $status = & $exe status server 2>&1
        if ($LASTEXITCODE -eq 0 -and (($status -join "`n") -match "status: running")) {
            break
        }
    } while ((Get-Date) -lt $deadline)

    if ((Get-Date) -ge $deadline) {
        throw "server did not become ready"
    }

    $savedErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $created = & $exe workspace create --cwd $PWD.Path 2>&1
        $createdExitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $savedErrorActionPreference
    }
    if ($createdExitCode -ne 0) {
        throw "workspace create failed with exit code $createdExitCode`: $($created -join "`n")"
    }
    $paneId = (($created -join "`n") | ConvertFrom-Json).result.root_pane.pane_id
    if ([string]::IsNullOrWhiteSpace($paneId)) {
        throw "workspace create did not return a root pane id: $($created -join "`n")"
    }
    $marker = "GOWILD_CONPTY_SMOKE_OK"

    $text = ""
    $attempt = 0
    $deadline = (Get-Date).AddSeconds(30)
    do {
        # Workspace creation can complete before the new shell is ready to
        # consume input. Re-sending this idempotent marker exercises the same
        # pane path without treating that startup race as a ConPTY failure.
        if (($attempt % 4) -eq 0) {
            Invoke-Checked $exe @("pane", "run", $paneId, "echo $marker")
        }
        Start-Sleep -Milliseconds 500
        try {
            $read = & $exe pane read $paneId --source recent-unwrapped --lines 40 --format text 2>&1
            $readExitCode = $LASTEXITCODE
        } catch {
            $read = @($_.Exception.Message)
            $readExitCode = 1
        }
        $text = $read -join "`n"
        if ($readExitCode -eq 0 -and (($text -replace "\s", "") -match $marker)) {
            break
        }
        $attempt += 1
    } while ((Get-Date) -lt $deadline)

    if (($text -replace "\s", "") -notmatch $marker) {
        throw "pane read did not include the smoke marker: $text"
    }
} finally {
    if ($null -ne $server) {
        try {
            $stopOutput = & $exe server stop 2>&1
            if ($LASTEXITCODE -ne 0) {
                Write-Host "server stop during cleanup exited with $LASTEXITCODE`: $($stopOutput -join "`n")"
            }
        } catch {
            Write-Host "server stop during cleanup failed: $($_.Exception.Message)"
        }
        $serverExitDeadline = (Get-Date).AddSeconds(10)
        do {
            $server.Refresh()
            if ($server.HasExited) {
                break
            }
            Start-Sleep -Milliseconds 100
        } while ((Get-Date) -lt $serverExitDeadline)
        $server.Refresh()
        if (-not $server.HasExited) {
            & taskkill.exe /PID $server.Id /T /F 2>&1 | Out-Null
        }
    }
    $global:LASTEXITCODE = 0
    $env:PATH = $oldPath
    if ($null -eq $oldSession) {
        Remove-Item Env:GOWILD_SESSION -ErrorAction SilentlyContinue
    } else {
        $env:GOWILD_SESSION = $oldSession
    }
    if ($null -eq $oldSocket) {
        Remove-Item Env:GOWILD_SOCKET_PATH -ErrorAction SilentlyContinue
    } else {
        $env:GOWILD_SOCKET_PATH = $oldSocket
    }
    if ($null -eq $oldClientSocket) {
        Remove-Item Env:GOWILD_CLIENT_SOCKET_PATH -ErrorAction SilentlyContinue
    } else {
        $env:GOWILD_CLIENT_SOCKET_PATH = $oldClientSocket
    }
    Remove-Item -Recurse -Force $fakeDir -ErrorAction SilentlyContinue
}
