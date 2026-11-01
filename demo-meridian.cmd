@echo off
rem Opens Meridian OS in a QEMU window and types a guided tour into its shell:
rem version and CPU info, reading files from the disk, running a program from
rem C:, a deliberately crashing program being stopped safely, and the full
rem kernel self-test suite. Screenshots of every step go to build\demo\.
rem The window stays open afterwards so you can keep exploring.
cd /d "%~dp0"
python tools\mbuild.py demo --release --quiet %*
pause
