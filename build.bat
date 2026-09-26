@echo off
chcp 65001 >nul
title Freebuff2API Build
cd /d "%~dp0"

echo ============================================
echo   Freebuff2API build script
echo ============================================
echo.

where cargo >nul 2>nul
if errorlevel 1 (
    echo [Error] cargo not found, please install Rust first: https://rustup.rs
    pause
    exit /b 1
)

echo [1/2] Building release version...
cargo build --release
if errorlevel 1 (
    echo [Error] Build failed
    pause
    exit /b 1
)

echo.
echo [2/2] Build succeeded!
echo   - Binary: target\release\freebuff2api.exe
echo   - Run:    start.bat
echo.
pause
