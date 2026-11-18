# Rebuilds convert_vs.cso, convert_y.cso and convert_uv.cso from convert.hlsl with the d3dcompiler that ships with Windows: powershell -File compile.ps1
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class Fxc {
    [DllImport("d3dcompiler_47.dll")]
    static extern int D3DCompile(byte[] src, UIntPtr len, string name, IntPtr defines, IntPtr include, string entry, string target, uint flags1, uint flags2, out IntPtr code, out IntPtr errors);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] delegate IntPtr GetPointer(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] delegate UIntPtr GetSize(IntPtr self);
    static byte[] Bytes(IntPtr blob) {
        IntPtr table = Marshal.ReadIntPtr(blob);
        var pointer = (GetPointer)Marshal.GetDelegateForFunctionPointer(Marshal.ReadIntPtr(table, 3 * IntPtr.Size), typeof(GetPointer));
        var size = (GetSize)Marshal.GetDelegateForFunctionPointer(Marshal.ReadIntPtr(table, 4 * IntPtr.Size), typeof(GetSize));
        int n = (int)size(blob).ToUInt64();
        byte[] bytes = new byte[n];
        Marshal.Copy(pointer(blob), bytes, 0, n);
        return bytes;
    }
    public static byte[] Compile(string source, string entry, string target) {
        IntPtr code, errors;
        byte[] text = System.Text.Encoding.UTF8.GetBytes(source);
        // Optimisation level 3, strictness, warnings as errors
        int hr = D3DCompile(text, (UIntPtr)text.Length, "convert.hlsl", IntPtr.Zero, IntPtr.Zero, entry, target, 0x8000u | 0x800u | 0x40000u, 0, out code, out errors);
        if (hr < 0) {
            string why = errors == IntPtr.Zero ? "" : System.Text.Encoding.UTF8.GetString(Bytes(errors));
            throw new Exception(entry + " did not compile (" + hr.ToString("x") + "): " + why);
        }
        return Bytes(code);
    }
}
"@
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$source = [IO.File]::ReadAllText((Join-Path $here 'convert.hlsl'))
foreach ($job in @(@('vs_main', 'vs_4_0', 'convert_vs.cso'), @('ps_y', 'ps_4_0', 'convert_y.cso'), @('ps_uv', 'ps_4_0', 'convert_uv.cso'))) {
    $bytes = [Fxc]::Compile($source, $job[0], $job[1])
    [IO.File]::WriteAllBytes((Join-Path $here $job[2]), $bytes)
    Write-Host ($job[2] + ': ' + $bytes.Length + ' bytes')
}
