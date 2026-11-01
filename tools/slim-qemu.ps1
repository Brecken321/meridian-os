# Frees disk space by removing QEMU emulators and firmware for CPU
# architectures Meridian does not use. Keeps qemu-system-x86_64, all DLLs,
# qemu-img and the x86-64 UEFI firmware, so `mbuild.py` keeps working.
# To undo, reinstall QEMU:  winget install SoftwareFreedomConservancy.QEMU
#
#   powershell -File tools\slim-qemu.ps1 -WhatIf   # list what would go
#   powershell -File tools\slim-qemu.ps1           # remove (asks for admin)
param(
    [string]$QemuDir = "C:\Program Files\qemu",
    [switch]$WhatIf
)

$keep = @('qemu-system-x86_64.exe', 'qemu-system-x86_64w.exe')
$remove = @()
$remove += Get-ChildItem $QemuDir -Filter 'qemu-system-*.exe' | Where-Object { $keep -notcontains $_.Name }
$remove += Get-ChildItem (Join-Path $QemuDir 'share') -Filter 'edk2-*' |
    Where-Object { $_.Name -notmatch '^edk2-(x86_64|i386)-' -and $_.Name -ne 'edk2-licenses.txt' }

$mb = ($remove | Measure-Object Length -Sum).Sum / 1MB
"{0} files, {1:N0} MB, can be removed from {2}" -f $remove.Count, $mb, $QemuDir
if ($WhatIf -or $remove.Count -eq 0) {
    $remove | ForEach-Object { "  " + $_.Name }
    return
}

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    "Asking Windows for administrator permission (QEMU lives in Program Files)..."
    $proc = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-QemuDir', "`"$QemuDir`"")
    exit $proc.ExitCode
}

$remove | Remove-Item -Force
"Removed {0:N0} MB. qemu-system-x86_64 is still installed." -f $mb
