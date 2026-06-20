@echo off
cd /d "%~dp0"

:: ── Set to 'false' to launch the app without showing this terminal window ──
set SHOW_TERMINAL=false

echo Telescope
echo =========
echo.

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

:: Download UnityCapture DLLs if missing
if not exist "unitycapture\UnityCaptureFilter64.dll" (
    echo Downloading virtual camera driver...
    if not exist "unitycapture" mkdir unitycapture
    powershell -NoProfile -Command "try { Invoke-WebRequest -Uri 'https://github.com/schellingb/UnityCapture/raw/master/Install/UnityCaptureFilter32.dll' -OutFile 'unitycapture\UnityCaptureFilter32.dll' -UseBasicParsing; Invoke-WebRequest -Uri 'https://github.com/schellingb/UnityCapture/raw/master/Install/UnityCaptureFilter64.dll' -OutFile 'unitycapture\UnityCaptureFilter64.dll' -UseBasicParsing; Write-Host 'Done.' } catch { Write-Host ('Failed: ' + $_.Exception.Message) }"
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
