# Loaded into every job windows_pass.py runs on the Windows desktop: finds and presses Telescope's controls through UI
# Automation and watches its windows from a background thread.
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class Watch {
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsHungAppWindow(IntPtr h);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int size);
    struct RECT { public int L, T, R, B; }

    static Thread thread;
    static volatile bool running;
    static List<string> log = new List<string>();
    static Dictionary<IntPtr, string> last = new Dictionary<IntPtr, string>();
    static Stopwatch clock;

    // Every visible top-level window of processes whose name starts with prefix: "hwnd|pid|title|l,t,r,b|hung"
    public static List<string> Windows(string prefix) {
        var pids = new HashSet<uint>();
        foreach (var p in Process.GetProcesses())
            if (p.ProcessName.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) pids.Add((uint)p.Id);
        var found = new List<string>();
        EnumWindows((h, l) => {
            uint pid;
            GetWindowThreadProcessId(h, out pid);
            int cloaked;
            if (pids.Contains(pid) && IsWindowVisible(h) && !(DwmGetWindowAttribute(h, 14, out cloaked, 4) == 0 && cloaked != 0)) {
                var sb = new StringBuilder(256);
                GetWindowText(h, sb, 256);
                RECT r;
                GetWindowRect(h, out r);
                found.Add(string.Format("{0}|{1}|{2}|{3},{4},{5},{6}|{7}", h.ToInt64(), pid, sb, r.L, r.T, r.R, r.B,
                                        IsHungAppWindow(h) ? 1 : 0));
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // Logs "seconds|hwnd|pid|title|rect|hung" whenever a window appears, moves, changes or goes ("gone")
    public static void Start(string prefix) {
        Stop();
        log = new List<string>();
        last = new Dictionary<IntPtr, string>();
        clock = Stopwatch.StartNew();
        running = true;
        thread = new Thread(() => {
            while (running) {
                var seen = new HashSet<IntPtr>();
                foreach (var w in Windows(prefix)) {
                    var h = new IntPtr(long.Parse(w.Split('|')[0]));
                    seen.Add(h);
                    var rest = w.Substring(w.IndexOf('|') + 1);
                    string before;
                    if (!last.TryGetValue(h, out before) || before != rest) {
                        last[h] = rest;
                        lock (log) log.Add(string.Format("{0:F2}|{1}", clock.Elapsed.TotalSeconds, w));
                    }
                }
                foreach (var h in new List<IntPtr>(last.Keys))
                    if (!seen.Contains(h)) {
                        last.Remove(h);
                        lock (log) log.Add(string.Format("{0:F2}|{1}|gone", clock.Elapsed.TotalSeconds, h.ToInt64()));
                    }
                Thread.Sleep(25);
            }
        });
        thread.IsBackground = true;
        thread.Start();
    }

    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, int x, int y, uint data, UIntPtr extra);

    public static void Click(int x, int y) {
        SetCursorPos(x, y);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);  // left down
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);  // left up
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);
    [DllImport("user32.dll")] static extern IntPtr GetDlgItem(IntPtr dlg, int id);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, string l);
    [DllImport("user32.dll")] static extern IntPtr PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

    // Type a path into a Windows Open dialog's name box and press Open
    public static bool OpenInFileDialog(string title, string path) {
        var dlg = FindWindow("#32770", title);
        if (dlg == IntPtr.Zero) return false;
        var edit = FindWindowEx(FindWindowEx(GetDlgItem(dlg, 1148), IntPtr.Zero, "ComboBox", null), IntPtr.Zero, "Edit", null);
        if (edit == IntPtr.Zero) return false;
        SendMessage(edit, 0x000C, IntPtr.Zero, path);  // WM_SETTEXT
        PostMessage(GetDlgItem(dlg, 1), 0x00F5, IntPtr.Zero, IntPtr.Zero);  // BM_CLICK on Open
        return true;
    }

    [DllImport("user32.dll")] static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool repaint);

    public static void Drag(int x1, int y1, int x2, int y2) {
        SetCursorPos(x1, y1);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
        for (int i = 1; i <= 30; i++) {  // in steps, or Windows never starts a drag
            SetCursorPos(x1 + (x2 - x1) * i / 30, y1 + (y2 - y1) * i / 30);
            Thread.Sleep(30);
        }
        Thread.Sleep(300);
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
    }

    public static void Place(long hwnd, int x, int y, int w, int h) { MoveWindow(new IntPtr(hwnd), x, y, w, h, true); }

    public static string[] Stop() {
        running = false;
        if (thread != null) thread.Join();
        thread = null;
        lock (log) return log.ToArray();
    }
}
'@

$UIA = [Windows.Automation.AutomationElement]
$Scope = [Windows.Automation.TreeScope]

function Wait-Until([scriptblock] $Test, [double] $Seconds = 20, [int] $StepMs = 250) {
    $end = (Get-Date).AddSeconds($Seconds)
    while ($true) {
        $got = & $Test
        if ($got -or (Get-Date) -gt $end) { return $got }
        Start-Sleep -Milliseconds $StepMs
    }
}

# Top-level UI Automation windows of processes whose name starts with $Prefix
function Get-AppWindows([string] $Prefix = "TelescopeDesktop") {
    $ids = @(Get-Process "$Prefix*" -ErrorAction SilentlyContinue | ForEach-Object Id)
    if (-not $ids) { return @() }
    @($UIA::RootElement.FindAll($Scope::Children, [Windows.Automation.Condition]::TrueCondition) |
        Where-Object { $ids -contains $_.Current.ProcessId })
}

# The first control named $Name (and of $Type, like "Button") in the app's windows, waiting up to $Seconds
function Find-Control([string] $Name, [string] $Type = "", [double] $Seconds = 20, [string] $Prefix = "TelescopeDesktop") {
    Wait-Until {
        $hidden = $null
        foreach ($w in Get-AppWindows $Prefix) {
            try { $all = @($w) + @($w.FindAll($Scope::Descendants, [Windows.Automation.Condition]::TrueCondition)) } catch { continue }  # it closed meanwhile
            foreach ($e in $all) {
                try {
                    if ($e.Current.Name -eq $Name -and (-not $Type -or $e.Current.ControlType.ProgrammaticName -eq "ControlType.$Type")) {
                        if (-not $e.Current.IsOffscreen) { return $e }
                        if (-not $hidden) { $hidden = $e }  # scrolled out of view, which pressing it through UI Automation doesn't mind
                    }
                } catch { }  # the element went away while looking
            }
        }
        $hidden
    } $Seconds
}

function Need-Control([string] $Name, [string] $Type = "", [double] $Seconds = 20, [string] $Prefix = "TelescopeDesktop") {
    $e = Find-Control $Name $Type $Seconds $Prefix
    if (-not $e) { throw "nothing called '$Name' $(if ($Type) { "($Type) " })showed up" }
    $e
}

$global:FirewallPrompts = 0

# Windows' firewall question ("allow public and private networks") swallows clicks elsewhere and UI Automation can't
# reach it from here, so close it the way a user dismissing it would leave things
function Close-FirewallPrompt {
    if ([Watch]::Windows("PickerHost") | Where-Object { $_ -like "*|Windows Security|*" }) {
        Get-Process PickerHost -ErrorAction SilentlyContinue | Stop-Process -Force
        $global:FirewallPrompts++
        Start-Sleep -Milliseconds 500
    }
}

function Click($Element) {
    Close-FirewallPrompt
    $r = $Element.Current.BoundingRectangle
    if ($r.Width -le 0) { throw "'$($Element.Current.Name)' isn't on screen to click" }
    [Watch]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
}

Add-Type -AssemblyName System.Windows.Forms

# Press a control: a real click when it's on screen, like a user (pressing through UI Automation doesn't switch Qt's
# settings pages, and a switch that opens a question from inside that call has crashed Qt), otherwise its pattern
function Press($Element) {
    $r = $Element.Current.BoundingRectangle
    $screen = [Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    if (-not $Element.Current.IsOffscreen -and $r.Width -gt 0 -and $r.Left -ge $screen.Left -and $r.Top -ge $screen.Top -and
        $r.Right -le $screen.Right -and $r.Bottom -le $screen.Bottom) { Click $Element; return }
    foreach ($p in $Element.GetSupportedPatterns()) {
        switch ($p.ProgrammaticName) {
            "InvokePatternIdentifiers.Pattern" { $Element.GetCurrentPattern($p).Invoke(); return }
            "TogglePatternIdentifiers.Pattern" { $Element.GetCurrentPattern($p).Toggle(); return }
            "SelectionItemPatternIdentifiers.Pattern" { $Element.GetCurrentPattern($p).Select(); return }
        }
    }
    Click $Element  # some of Windows' own dialogs only take a real click
}

function Press-Control([string] $Name, [string] $Type = "", [double] $Seconds = 20, [string] $Prefix = "TelescopeDesktop") {
    Press (Need-Control $Name $Type $Seconds $Prefix)
}

function Is-On($Element) {
    $Element.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq "On"
}

function Set-Text($Element, [string] $Text) {
    $Element.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue($Text)
}

# Pick $Item in a combo box
function Choose([string] $Combo, [string] $Item) {
    $c = Need-Control $Combo "ComboBox"
    $c.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Press (Need-Control $Item "ListItem" 5)
}

# Names of every text, button and other named control in the app's windows, for "does it say X" checks
function Get-Texts([string] $Prefix = "TelescopeDesktop") {
    foreach ($w in Get-AppWindows $Prefix) {
        try { $all = $w.FindAll($Scope::Descendants, [Windows.Automation.Condition]::TrueCondition); $w.Current.Name } catch { continue }
        foreach ($e in $all) {
            try { if ($e.Current.Name -and -not $e.Current.IsOffscreen) { $e.Current.Name } } catch { }
        }
    }
}

function Close-Window([string] $Title, [string] $Prefix = "TelescopeDesktop") {
    $w = Need-Control $Title "Window" 10 $Prefix
    $w.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern).Close()
}

# Start like a user would from Explorer (so not elevated, and not a child of this job)
function Open-AsUser([string] $Path) {
    Start-Process explorer.exe -ArgumentList "`"$Path`""
}

function Main-Window([double] $Seconds = 120) {
    Wait-Until { Get-AppWindows | Where-Object { $_.Current.Name -eq "Telescope" -and $_.Current.AutomationId -like "*TelescopeWindow*" } |
                 Select-Object -First 1 } $Seconds 500
}

# Do something that restarts Telescope (an update, Restart now) and wait for its window to be back
function Wait-Restart([scriptblock] $Action, [double] $Seconds = 600) {
    $old = @(Get-Process TelescopeDesktop -ErrorAction SilentlyContinue | ForEach-Object Id)
    & $Action
    if (-not (Wait-Until { -not (Get-Process -Id $old -ErrorAction SilentlyContinue) } $Seconds 1)) { throw "Telescope didn't close to restart" }
    if (-not (Main-Window $Seconds)) { throw "Telescope's window didn't come back" }
    Start-Sleep 3
}
