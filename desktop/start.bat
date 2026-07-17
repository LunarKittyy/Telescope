@echo off
cd /d "%~dp0"

:: ── Set to 'false' to launch the app without showing this terminal window ──
set SHOW_TERMINAL=false

echo Telescope
echo =========
echo.

:: Prebuilt EXE release: just launch it directly, no Python required.
if exist "TelescopeDesktop.exe" (
    echo Launching Telescope...
    echo.
    start "" "TelescopeDesktop.exe"
    exit /b 0
)

:: Fallback: running from the Python source tree (not the EXE release).
if not exist "main.py" (
    echo Neither TelescopeDesktop.exe nor main.py was found next to this script.
    echo This launcher only works inside a Telescope release folder.
    pause
    exit /b 1
)
if not exist "requirements.txt" (
    echo main.py was found but requirements.txt is missing - this doesn't look
    echo like a complete Telescope source checkout.
    pause
    exit /b 1
)

:: Check Python
python --version >nul 2>&1
if errorlevel 1 (
    echo Python 3 is required but was not found.
    echo Download it from https://www.python.org/downloads/
    echo During installation, check "Add Python to PATH".
    echo.
    start https://www.python.org/downloads/
    pause
    exit /b 1
)

:: Install Python dependencies
echo Checking dependencies...
python -m pip install --quiet -r requirements.txt
echo.

:: Download UnityCapture DLLs if missing. Pinned to a specific commit (not the
:: mutable master branch) and checksum-verified, since telescope.platform.windows
:: also registers these with admin rights (regsvr32) - see UNITYCAPTURE_URL_BASE
:: / _EXPECTED_SHA256 there, which this mirrors.
if not exist "unitycapture\UnityCaptureFilter64.dll" (
    echo Downloading virtual camera driver...
    if not exist "unitycapture" mkdir unitycapture
    powershell -NoProfile -Command "$commit = '3ed54c325e0ad71afcf4f246c07e5e17b3d7f2d2'; $hashes = @{ 'UnityCaptureFilter32.dll' = 'aa3ebdf03dea7f3aab3dd7b724751f49ed71672256b57c6a19aa6809cabf30ba'; 'UnityCaptureFilter64.dll' = '72812f5363d8ecb45632253f8c8c888844b1b62e27616f3c8cc21064ccde25e5' }; try { foreach ($name in $hashes.Keys) { $dest = 'unitycapture\' + $name; $url = 'https://raw.githubusercontent.com/schellingb/UnityCapture/' + $commit + '/Install/' + $name; Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing; $actual = (Get-FileHash -Path $dest -Algorithm SHA256).Hash.ToLower(); if ($actual -ne $hashes[$name]) { Remove-Item $dest -Force; throw ($name + ' failed checksum verification') } }; Write-Host 'Done.' } catch { Write-Host ('Failed: ' + $_.Exception.Message) }"
    echo.
)

:: Launch
if /i "%SHOW_TERMINAL%"=="true" (
    echo Launching Telescope...
    echo.
    python main.py
    if errorlevel 1 (
        echo.
        echo Telescope exited with an error.
        pause
    )
) else (
    start "" pythonw main.py
)
