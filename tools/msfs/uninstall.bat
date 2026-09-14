@echo off
rem Removes the AMDB Airport Moving Map again and puts back anything it displaced.
rem Nothing else is touched: no aircraft files were modified, and nothing was written to
rem the registry or the hosts file.
setlocal enabledelayedexpansion
title AMDB Airport Moving Map - uninstall

set "FOUND=0"
call :remove "%LOCALAPPDATA%\Packages\Microsoft.FlightSimulator_8wekyb3d8bbwe\LocalCache\UserCfg.opt" "MSFS 2020 (Microsoft Store)"
call :remove "%APPDATA%\Microsoft Flight Simulator\UserCfg.opt" "MSFS 2020 (Steam)"
call :remove "%LOCALAPPDATA%\Packages\Microsoft.Limitless_8wekyb3d8bbwe\LocalCache\UserCfg.opt" "MSFS 2024 (Microsoft Store)"
call :remove "%APPDATA%\Microsoft Flight Simulator 2024\UserCfg.opt" "MSFS 2024 (Steam)"

echo.
if "%FOUND%"=="0" (
    echo   The map was not installed in any Community folder found.
) else (
    echo   Done. Restart the sim for the change to take effect.
)
echo.
pause
exit /b 0

:remove
if not exist %1 exit /b 0
set "PKG="
for /f "tokens=1,* delims= " %%a in ('findstr /b /c:"InstalledPackagesPath" %1') do set "PKG=%%~b"
if not defined PKG exit /b 0
set PKG=!PKG:"=!
set "COMM=!PKG!\Community"
if not exist "!COMM!" exit /b 0

rem Written without parenthesised blocks on purpose: the sim names below contain a closing
rem bracket, which would end an `if (...)` block early and cut the line short.
if not exist "!COMM!\zzz-amdb-a220-amm" exit /b 0
set "FOUND=1"
echo.
echo   %~2
rmdir /s /q "!COMM!\zzz-amdb-a220-amm"
echo     removed

rem Put back whatever was set aside to make room for it.
if not exist "!PKG!\_disabled\zzz-gm5-a220-amm" exit /b 0
move /y "!PKG!\_disabled\zzz-gm5-a220-amm" "!COMM!\" >nul 2>&1
echo     restored the GM5 map
exit /b 0
