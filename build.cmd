@echo off
REM Build the merged plugin workspace under a consistent MSVC environment.
REM adblock's rustls -> ring compiles C code; cc-rs must use the same toolset
REM as vcvars or cl reports "Command line error D8050".
REM
REM Usage: build.cmd [cargo args...]
REM   build.cmd                  -> cargo build --workspace
REM   build.cmd --offline        -> cargo build --workspace --offline
REM   build.cmd test --workspace -> cargo test --workspace
call "E:\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" || exit /b 1
set "CC=E:\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64\cl.exe"
REM Drop cc-rs' default flags (/Z7 trips cl D8050 here), but keep /MD so the C
REM objects match Rust's CRT (bare cl defaults to /MT -> LNK4098).
set CRATE_CC_NO_DEFAULTS=1
set CFLAGS_x86_64_pc_windows_msvc=/MD
cd /d "%~dp0"
set "first=%~1"
if "%first%"=="" (
  cargo build --workspace
  exit /b %errorlevel%
)
if "%first:~0,1%"=="-" (
  cargo build --workspace %*
  exit /b %errorlevel%
)
cargo %*
