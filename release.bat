@echo off
setlocal
cd /d "%~dp0"

rem 从 Cargo.toml 读取版本号
set "VER="
for /f "tokens=2 delims== " %%a in ('findstr /b /c:"version" Cargo.toml') do if not defined VER set "VER=%%a"
set "VER=%VER:"=%"
if not defined VER (
    echo 未能从 Cargo.toml 读取版本号。
    exit /b 1
)

rem 校验 CHANGELOG 有对应条目
findstr /c:"## v%VER%" CHANGELOG.md >nul 2>&1
if errorlevel 1 (
    echo CHANGELOG.md 里没有 "## v%VER%" 条目，请先补充更新说明。
    exit /b 1
)

rem tag 不能已存在
git tag -l v%VER% | findstr . >nul
if not errorlevel 1 (
    echo 标签 v%VER% 已存在，请先升版本号。
    exit /b 1
)

echo [1/4] 编译 v%VER% ...
call build.bat
if errorlevel 1 exit /b 1

echo [2/4] 提交并打标签 ...
git add -A
git diff --cached --quiet || git commit -m "release: v%VER%"
git tag v%VER%
if errorlevel 1 exit /b 1

echo [3/4] 推送代码与标签 ...
git push 2>&1 | findstr /v "warning:"
git push origin v%VER% 2>&1 | findstr /v "warning:"

echo [4/4] 创建 GitHub Release ...
copy /y "bin\proxpilot.exe" "%TEMP%\proxpilot-v%VER%.exe" >nul
gh release create v%VER% "%TEMP%\proxpilot-v%VER%.exe" --title "ProxPilot v%VER% · 代理优选助手" --notes-file CHANGELOG.md

endlocal
