# 2026-10-27 Updates on Linux and Windows

0.1.6's self-update was proven on macOS against the real release service; Linux and Windows were covered by
unit tests only. This round proves Linux in a real desktop session, and does what can be done for Windows without
a Windows machine.

## Linux: proven

`scripts/test-linux-update.sh` (new) runs in Docker on this machine's architecture (arm64 here, natively):
Ubuntu 24.04, a virtual display, D-Bus and an unlocked keyring (`scripts/linux-desktop-session.sh`), an ordinary
user. `install.sh` from haarness.ai installed the released 0.1.6 AppImage with its `haarness-desktop` launcher;
without libfuse2 (Ubuntu's default) the launcher runs it with `APPIMAGE_EXTRACT_AND_RUN=1`, and the app still sees
`APPIMAGE`, so it updates itself in place. A stand-in release service on loopback offered "9.9.9", whose file was
the real 0.1.5 AppImage on dl.haarness.ai with its published size and SHA-256. Settings → About checked, the
download (from the release domain) was staged beside the app with matching bytes, **Restart to update** quit, the
helper replaced `~/.local/bin/Haarness.AppImage` by one rename (nothing left beside it), wrote
`{"ok":true}`, and reopened it with its flags (`--no-sandbox`, which the container needs); it was still running
10 s later, and opened again it ran 0.1.5, the file it was given. Run twice, both passed.

## Windows: what changed, and what is proven

Reading electron-builder's NSIS templates showed that the setup, run with `--updated /S`, ends every process whose
program lies in the install folder (PowerShell's `Get-CimInstance`, then `Stop-Process -Force`), after about two
seconds. That includes Desktop's own servers (`resources\bin\haarness.exe`) and what they run, which 0.1.6 let it
kill abruptly, and the docs wrongly said chats keep running through an update on every system. Windows cannot
replace a running program, so the servers must stop; they now stop on their own terms:

- `WINDOWS_HELPER` (Windows PowerShell 5.1, every Windows 10 and 11; inputs in the environment, the script passed
  `-EncodedCommand`) waits for Haarness to exit, asks each server whose program is in Desktop's `resources\bin` to
  stop (`POST /v1/shutdown` with its record's token, as `haarness serve --stop` does) and waits up to 15 s, runs the
  setup as electron-updater does (`--updated /S --force-run`), and writes the outcome for the next launch, which
  0.1.6 lacked on Windows: a failed setup (Smart App Control refusing the unsigned file, say) is reported in About
  and the old Haarness opens again. Without Windows PowerShell the setup runs alone, as before.
- Renames retry for a few seconds on `EPERM`/`EACCES`/`EBUSY`, which an antivirus scan of a new `.exe` causes.
- Docs (DESKTOP.md and haarness.ai's install page) say that on Windows a running reply stops while chats and their
  history stay.

Proven: `scripts/test-windows-update-helper.py` (new) runs the helper's own text under PowerShell 7 (natively,
arm64) with stand-ins: it waits for Haarness, stops only the server in the bin folder (its record naming
`0.0.0.0`, asked on loopback with its token) and only before the setup runs, leaves a server elsewhere alone,
passes `--updated /S --force-run`, and reports success; a setup that fails is reported and Haarness opens again.
Unit tests check the PowerShell command (path, flags, the encoded script decoding to the helper, the inputs).

Not proven: the setup itself. Under Wine in Docker on this Mac the 32-bit NSIS stub runs through `qemu-i386` and
made no progress in 18 minutes, and no Windows machine is at hand. Windows PowerShell 5.1 specifics (the helper
was run under 7.4) and the setup's `--updated /S --force-run` path remain to be seen on Windows.

Desktop unit tests 628, the Desktop update e2e 4/4. These changes ship with the next release; 0.1.6 copies on
Windows update to it the 0.1.6 way (the setup without the graceful stop).
