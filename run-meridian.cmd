@echo off
rem Builds Meridian OS and boots it in a QEMU window (nothing is installed
rem on your computer; everything stays in this folder's build\ and target\).
rem
rem Click inside the QEMU window to type into the Meridian shell, then try:
rem   help, ver, cpu, mem, threads, ps, run init, dir, type, shutdown
rem Press Ctrl+Alt+G to release the mouse from the QEMU window.
rem
rem Extra options are passed through, for example:
rem   run-meridian.cmd --smp 2 --memory 1G
cd /d "%~dp0"
python tools\mbuild.py run --release %*
if errorlevel 1 pause
