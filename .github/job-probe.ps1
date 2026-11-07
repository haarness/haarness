# Does Git Bash's background work stay in a job object, and does closing or
# terminating the job end it? Prints the runner's own job flags, then for each
# way of stopping: which sh/sleep processes ran and whether they were in the
# job, and whether the background work outlived the stop.
$ErrorActionPreference = 'Stop'
$src = @'
using System;
using System.Runtime.InteropServices;
public static class Jobs {
  [StructLayout(LayoutKind.Sequential)] public struct BASIC { public long a; public long b; public uint LimitFlags; public UIntPtr c; public UIntPtr d; public uint e; public UIntPtr f; public uint g; public uint h; }
  [StructLayout(LayoutKind.Sequential)] public struct IO { public ulong a, b, c, d, e, f; }
  [StructLayout(LayoutKind.Sequential)] public struct EXT { public BASIC Basic; public IO Io; public UIntPtr a, b, c, d; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct SI { public int cb; public string r, d, t; public int x, y, xs, ys, xc, yc, fill, flags; public short show, r2; public IntPtr r3, i, o, e; }
  [StructLayout(LayoutKind.Sequential)] public struct PI { public IntPtr Process, Thread; public int Pid, Tid; }
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern IntPtr CreateJobObjectW(IntPtr a, string n);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool SetInformationJobObject(IntPtr j, int c, ref EXT i, int l);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool QueryInformationJobObject(IntPtr j, int c, out EXT i, int l, IntPtr r);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AssignProcessToJobObject(IntPtr j, IntPtr p);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr p, IntPtr j, out bool r);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool TerminateJobObject(IntPtr j, uint code);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint a, bool i, int pid);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern bool CreateProcessW(string app, System.Text.StringBuilder cmd, IntPtr pa, IntPtr ta, bool inh, uint flags, IntPtr env, string cwd, ref SI si, out PI pi);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern uint ResumeThread(IntPtr t);
  [DllImport("kernel32.dll")] public static extern IntPtr GetCurrentProcess();
  public static string OwnJob() {
    bool inJob; IsProcessInJob(GetCurrentProcess(), IntPtr.Zero, out inJob);
    if (!inJob) return "this step runs in no job";
    EXT e; QueryInformationJobObject(IntPtr.Zero, 9, out e, Marshal.SizeOf(typeof(EXT)), IntPtr.Zero);
    return String.Format("this step's job limit flags 0x{0:x} (breakaway ok 0x800, silent 0x1000, kill on close 0x2000)", e.Basic.LimitFlags);
  }
  public static IntPtr NewJob() {
    var j = CreateJobObjectW(IntPtr.Zero, null);
    var e = new EXT(); e.Basic.LimitFlags = 0x2000;
    if (!SetInformationJobObject(j, 9, ref e, Marshal.SizeOf(typeof(EXT)))) throw new Exception("set " + Marshal.GetLastWin32Error());
    return j;
  }
  public static int Start(IntPtr job, string cmd, string cwd) {
    var si = new SI(); si.cb = Marshal.SizeOf(typeof(SI)); PI pi;
    if (!CreateProcessW(null, new System.Text.StringBuilder(cmd), IntPtr.Zero, IntPtr.Zero, false, 0x4 | 0x08000000, IntPtr.Zero, cwd, ref si, out pi)) throw new Exception("create " + Marshal.GetLastWin32Error());
    if (!AssignProcessToJobObject(job, pi.Process)) throw new Exception("assign " + Marshal.GetLastWin32Error());
    ResumeThread(pi.Thread); CloseHandle(pi.Thread); CloseHandle(pi.Process);
    return pi.Pid;
  }
  public static string InJob(IntPtr job, int pid) {
    var h = OpenProcess(0x1000, false, pid);
    if (h == IntPtr.Zero) return "gone";
    bool r; IsProcessInJob(h, job, out r); CloseHandle(h);
    return r ? "in the job" : "OUTSIDE the job";
  }
}
'@
Add-Type -TypeDefinition $src
[Jobs]::OwnJob()
$env:CHERE_INVOKING = '1'
$bash = 'C:\Program Files\Git\bin\bash.exe'
foreach ($way in 'close', 'terminate', 'taskkill') {
  $dir = Join-Path $env:RUNNER_TEMP "jobprobe-$way"
  New-Item -ItemType Directory -Force $dir | Out-Null
  $job = [Jobs]::NewJob()
  $leader = [Jobs]::Start($job, "`"$bash`" -lc `"sh -c 'sleep 6; touch escaped' & printf ready > ready; wait`"", $dir)
  $deadline = (Get-Date).AddSeconds(30)
  while (-not (Test-Path (Join-Path $dir 'ready')) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 50 }
  Start-Sleep -Milliseconds 300
  "--- $way (leader $leader)"
  Get-CimInstance Win32_Process | Where-Object { $_.Name -in 'sh.exe', 'sleep.exe', 'bash.exe' } |
    ForEach-Object { "  $($_.ProcessId) parent $($_.ParentProcessId) $($_.Name): $([Jobs]::InJob($job, [int]$_.ProcessId))" }
  switch ($way) {
    'close' { [Jobs]::CloseHandle($job) | Out-Null }
    'terminate' { [Jobs]::TerminateJobObject($job, 1) | Out-Null; [Jobs]::CloseHandle($job) | Out-Null }
    'taskkill' { taskkill /F /T /PID $leader 2>&1 | ForEach-Object { "  $_" } }
  }
  Start-Sleep -Milliseconds 7000
  "  outlived the stop: $(Test-Path (Join-Path $dir 'escaped'))"
  if ($way -eq 'taskkill') { [Jobs]::CloseHandle($job) | Out-Null }
}
