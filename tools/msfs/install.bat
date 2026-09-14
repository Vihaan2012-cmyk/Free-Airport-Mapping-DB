@echo off
rem Installs the AMDB Airport Moving Map into every Microsoft Flight Simulator found.
rem The Community folder is read from each sim's UserCfg.opt, so a moved package library
rem is picked up too and nobody has to go hunting for the path.
setlocal enabledelayedexpansion
title AMDB Airport Moving Map - install

set "SRC=%~dp0msfs\zzz-amdb-a220-amm"
if not exist "%SRC%\manifest.json" (
    echo.
    echo   Could not find  msfs\zzz-amdb-a220-amm  next to this script.
    echo   Unzip the whole download, then run install.bat from inside the unzipped folder.
    echo.
    pause
    exit /b 1
)

set "FOUND=0"
call :install "%LOCALAPPDATA%\Packages\Microsoft.FlightSimulator_8wekyb3d8bbwe\LocalCache\UserCfg.opt" "MSFS 2020 (Microsoft Store)"
call :install "%APPDATA%\Microsoft Flight Simulator\UserCfg.opt" "MSFS 2020 (Steam)"
call :install "%LOCALAPPDATA%\Packages\Microsoft.Limitless_8wekyb3d8bbwe\LocalCache\UserCfg.opt" "MSFS 2024 (Microsoft Store)"
call :install "%APPDATA%\Microsoft Flight Simulator 2024\UserCfg.opt" "MSFS 2024 (Steam)"

echo.
if "%FOUND%"=="0" (
    echo   No Microsoft Flight Simulator installation was found.
    echo   Copy  msfs\zzz-amdb-a220-amm  into your Community folder by hand.
) else (
    echo   Done. Now:
    echo     1. run   amdb-bridge.exe serve --no-hosts --no-patch     and leave it running
    echo     2. start the sim and load the Synaptic A220
    echo     3. the map appears by itself once you are on the ground
)
echo.
pause
exit /b 0

:install
if not exist %1 exit /b 0
set "PKG="
for /f "tokens=1,* delims= " %%a in ('findstr /b /c:"InstalledPackagesPath" %1') do set "PKG=%%~b"
if not defined PKG exit /b 0
set PKG=!PKG:"=!
set "COMM=!PKG!\Community"
if not exist "!COMM!" exit /b 0
set "FOUND=1"
echo.
echo   %~2
echo     !COMM!

rem Both maps replace the same display-unit file, so only one of them can load.
if exist "!COMM!\zzz-gm5-a220-amm" (
    if not exist "!PKG!\_disabled" mkdir "!PKG!\_disabled" >nul 2>&1
    move /y "!COMM!\zzz-gm5-a220-amm" "!PKG!\_disabled\" >nul 2>&1
    echo     set the GM5 map aside in _disabled
)
if exist "!COMM!\zzz-amdb-a220-amm" rmdir /s /q "!COMM!\zzz-amdb-a220-amm"
xcopy "%SRC%" "!COMM!\zzz-amdb-a220-amm\" /e /i /q /y >nul
if errorlevel 1 (echo     COPY FAILED - is the sim running?) else (echo     installed)
exit /b 0
