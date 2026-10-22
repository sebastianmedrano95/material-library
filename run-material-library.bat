@echo off
setlocal
cd /d "%~dp0"

set "CARGO_EXE=%USERPROFILE%\.cargo\bin\cargo.exe"
if exist "%CARGO_EXE%" (
    "%CARGO_EXE%" run --offline
) else (
    cargo run --offline
)

if errorlevel 1 (
    echo.
    echo Material Library no pudo iniciarse. Revisa el mensaje anterior.
    pause
)
