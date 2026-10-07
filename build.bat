@echo off
setlocal
cd /d "%~dp0"
call build-env.bat

echo [1/2] cargo build --release ...
cargo build --release
if errorlevel 1 (
    echo 编译失败。
    exit /b 1
)

if not exist bin mkdir bin

rem 兼容两种产物路径：默认路径优先；全局配置了 build.target 时带 triple
set "SRC="
if exist "target\release\proxpilot.exe" set "SRC=target\release\proxpilot.exe"
if not defined SRC if exist "target\x86_64-pc-windows-msvc\release\proxpilot.exe" set "SRC=target\x86_64-pc-windows-msvc\release\proxpilot.exe"
if not defined SRC (
    echo 未找到编译产物，请检查 target 目录。
    exit /b 1
)

copy /y "%SRC%" "bin\proxpilot.exe" >nul
echo [2/2] 已输出：bin\proxpilot.exe
endlocal
