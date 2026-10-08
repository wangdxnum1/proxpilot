@echo off
setlocal
cd /d "%~dp0"
call build-env.bat

echo [1/2] cargo build --release ...
cargo build --release
if errorlevel 1 (
    echo Build failed.
    exit /b 1
)

if not exist bin mkdir bin

rem Support the default output path and the Windows MSVC target path.
set "SRC="
if exist "target\release\proxpilot.exe" set "SRC=target\release\proxpilot.exe"
if not defined SRC if exist "target\x86_64-pc-windows-msvc\release\proxpilot.exe" set "SRC=target\x86_64-pc-windows-msvc\release\proxpilot.exe"
if not defined SRC (
    echo Build output not found. Check the target directory.
    exit /b 1
)

copy /y "%SRC%" "bin\proxpilot.exe" >nul
echo [2/2] Output: bin\proxpilot.exe
endlocal
