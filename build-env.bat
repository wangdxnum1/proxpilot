@echo off
rem Build-only tools: never required by proxpilot.exe at runtime.
if exist "%~dp0target\build-tools\nasm-2.16.03\nasm.exe" set "PATH=%~dp0target\build-tools\nasm-2.16.03;%PATH%"
if not defined LIBCLANG_PATH if exist "%ProgramFiles%\LLVM\bin\libclang.dll" set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"
if not defined LIBCLANG_PATH if exist "%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" (
    for /f "usebackq delims=" %%I in (`"%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -find VC\Tools\Llvm\x64\bin\libclang.dll`) do set "LIBCLANG_PATH=%%~dpI"
)
