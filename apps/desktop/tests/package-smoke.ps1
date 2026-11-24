param(
    [Parameter(Mandatory)]
    [string] $Installer
)

$ErrorActionPreference = 'Stop'
$installerPath = (Resolve-Path $Installer).Path
$directory = Join-Path $env:RUNNER_TEMP ('gamesync-install-' + [Guid]::NewGuid())
$database = Join-Path $env:LOCALAPPDATA 'io.gamesync.desktop\client.sqlite'
if (Test-Path $database) {
    throw 'Use a fresh Windows test account to verify the installer.'
}

# NSIS requires the unquoted /D argument at the end of the raw command line.
$setup = Start-Process -FilePath $installerPath -ArgumentList "/S /D=$directory" -PassThru
if (-not $setup.WaitForExit(180000)) {
    Stop-Process -Id $setup.Id -Force
    throw 'GameSync installation timed out.'
}
if ($setup.ExitCode -ne 0) {
    throw "GameSync installation failed with exit code $($setup.ExitCode)."
}
$executable = Join-Path $directory 'gamesync-desktop.exe'
if (-not (Test-Path $executable)) {
    throw "The installer did not write $executable."
}

$client = Start-Process -FilePath $executable -PassThru
try {
    $deadline = (Get-Date).AddSeconds(45)
    while ((Get-Date) -lt $deadline) {
        if ($client.HasExited) {
            throw "The installed client exited with code $($client.ExitCode)."
        }
        if (Test-Path $database) {
            $tables = python -c 'import sqlite3, sys; print(" ".join(row[0] for row in sqlite3.connect(sys.argv[1]).execute("SELECT name FROM sqlite_master")))' $database
            $names = $tables -split ' '
            if ($LASTEXITCODE -eq 0 -and ($names -contains 'activity') -and ($names -contains 'config')) {
                Write-Output 'The Windows installer launched GameSync and the webview initialized its activity database.'
                break
            }
        }
        Start-Sleep -Milliseconds 250
    }
    if ((Get-Date) -ge $deadline) {
        throw 'The installed GameSync client did not initialize its database.'
    }
}
finally {
    if (-not $client.HasExited) {
        Stop-Process -Id $client.Id -Force
        $client.WaitForExit()
    }
}

$hash = (Get-FileHash $database).Hash
$uninstaller = Join-Path $directory 'uninstall.exe'
$uninstall = Start-Process -FilePath $uninstaller -ArgumentList "/S _?=$directory" -PassThru
if (-not $uninstall.WaitForExit(60000) -or $uninstall.ExitCode -ne 0 -or (Test-Path $executable)) {
    throw 'GameSync uninstall failed or timed out.'
}
if ((Get-FileHash $database).Hash -ne $hash) {
    throw 'Uninstall changed the GameSync database.'
}
$reinstall = Start-Process -FilePath $installerPath -ArgumentList "/S /D=$directory" -PassThru
if (-not $reinstall.WaitForExit(180000) -or $reinstall.ExitCode -ne 0 -or -not (Test-Path $executable)) {
    throw 'GameSync reinstall failed or timed out.'
}
if ((Get-FileHash $database).Hash -ne $hash) {
    throw 'Reinstall changed the GameSync database.'
}
