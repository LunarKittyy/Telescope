// COM entry points, and registering the cameras with Windows (regsvr32, as admin).
#include "camera.h"

#include <stdio.h>
#include <wchar.h>

namespace telescope {

std::atomic<long> g_objects{0};
HMODULE g_module = nullptr;

namespace {

// One CLSID per camera slot; desktop/telescope/platform/windows.py has the same list
const GUID kClsids[kSlots] = {
    {0xD1063481, 0x2B46, 0x4750, {0x8A, 0x9D, 0x85, 0xF6, 0x95, 0x73, 0x35, 0x5A}},
    {0x563D2A25, 0x6CA0, 0x48AF, {0x92, 0x2D, 0xB0, 0x98, 0x03, 0x58, 0x93, 0xB4}},
    {0x8DF9E1B0, 0x1CD9, 0x42D3, {0x86, 0xF1, 0xED, 0x26, 0x91, 0xC3, 0xD1, 0x97}},
    {0x128E9FE3, 0x4FDD, 0x4786, {0x87, 0xE1, 0xEE, 0xA1, 0x2E, 0x34, 0x25, 0x4A}},
};

// What apps list each slot's camera as, the way vcam.slot_label names them
void SlotName(int slot, wchar_t* out, size_t size) {
    if (slot == 0) {
        wcscpy_s(out, size, L"Telescope");
    } else {
        swprintf_s(out, size, L"Telescope #%d", slot + 1);
    }
}

class Factory : public RuntimeClass<RuntimeClassFlags<ClassicCom>, IClassFactory> {
public:
    explicit Factory(int slot) : m_slot(slot) {}

    IFACEMETHODIMP CreateInstance(IUnknown* outer, REFIID riid, void** object) override {
        if (!object) return E_POINTER;
        *object = nullptr;
        if (outer) return CLASS_E_NOAGGREGATION;
        ComPtr<SourceActivate> activate;
        HRESULT hr = Microsoft::WRL::MakeAndInitialize<SourceActivate>(&activate, m_slot);
        return FAILED(hr) ? hr : activate.CopyTo(riid, object);
    }

    IFACEMETHODIMP LockServer(BOOL lock) override {
        lock ? ++g_objects : --g_objects;
        return S_OK;
    }

private:
    int m_slot;
    ModuleRef m_ref;
};

HRESULT RegisterClsid(int slot) {
    wchar_t path[MAX_PATH];
    DWORD length = GetModuleFileNameW(g_module, path, MAX_PATH);
    if (!length || length >= MAX_PATH) return HRESULT_FROM_WIN32(ERROR_FILENAME_EXCED_RANGE);
    wchar_t clsid[40], key[128], name[32];
    StringFromGUID2(kClsids[slot], clsid, ARRAYSIZE(clsid));
    swprintf_s(key, L"SOFTWARE\\Classes\\CLSID\\%s\\InprocServer32", clsid);
    SlotName(slot, name, ARRAYSIZE(name));
    HKEY handle = nullptr;
    LSTATUS status = RegCreateKeyExW(HKEY_LOCAL_MACHINE, key, 0, nullptr, 0, KEY_SET_VALUE | KEY_WOW64_64KEY, nullptr,
                                     &handle, nullptr);
    if (status != ERROR_SUCCESS) return HRESULT_FROM_WIN32(status);
    status = RegSetValueExW(handle, nullptr, 0, REG_SZ, reinterpret_cast<const BYTE*>(path), (length + 1) * sizeof(wchar_t));
    if (status == ERROR_SUCCESS) {
        static const wchar_t both[] = L"Both";
        status = RegSetValueExW(handle, L"ThreadingModel", 0, REG_SZ, reinterpret_cast<const BYTE*>(both), sizeof(both));
    }
    RegCloseKey(handle);
    if (status != ERROR_SUCCESS) return HRESULT_FROM_WIN32(status);
    swprintf_s(key, L"SOFTWARE\\Classes\\CLSID\\%s", clsid);
    status = RegSetKeyValueW(HKEY_LOCAL_MACHINE, key, nullptr, REG_SZ, name, static_cast<DWORD>((wcslen(name) + 1) * sizeof(wchar_t)));
    if (status == ERROR_SUCCESS) {
        status = RegSetKeyValueW(HKEY_LOCAL_MACHINE, key, L"TelescopeVersion", REG_DWORD, &kCameraVersion, sizeof(kCameraVersion));
    }
    return HRESULT_FROM_WIN32(status);
}

void UnregisterClsid(int slot) {
    wchar_t clsid[40], key[128];
    StringFromGUID2(kClsids[slot], clsid, ARRAYSIZE(clsid));
    swprintf_s(key, L"SOFTWARE\\Classes\\CLSID\\%s", clsid);
    RegDeleteTreeW(HKEY_LOCAL_MACHINE, key);
    RegDeleteKeyW(HKEY_LOCAL_MACHINE, key);
}

HRESULT CreateCamera(int slot, IMFVirtualCamera** camera) {
    wchar_t clsid[40], name[32];
    StringFromGUID2(kClsids[slot], clsid, ARRAYSIZE(clsid));
    SlotName(slot, name, ARRAYSIZE(name));
    return MFCreateVirtualCamera(MFVirtualCameraType_SoftwareCameraSource, MFVirtualCameraLifetime_System,
                                 MFVirtualCameraAccess_AllUsers, name, clsid, nullptr, 0, camera);
}

// Lasts across restarts until it's removed, for every account on the computer
HRESULT AddCamera(int slot) {
    ComPtr<IMFVirtualCamera> camera;
    HRESULT hr = CreateCamera(slot, &camera);
    if (SUCCEEDED(hr)) hr = camera->Start(nullptr);
    if (camera) camera->Shutdown();
    Log(L"adding camera %d: 0x%08x", slot, hr);
    return hr;
}

void RemoveCamera(int slot) {
    ComPtr<IMFVirtualCamera> camera;
    HRESULT hr = CreateCamera(slot, &camera);
    if (SUCCEEDED(hr)) hr = camera->Remove();
    if (camera) camera->Shutdown();
    Log(L"removing camera %d: 0x%08x", slot, hr);
}

// count cameras afterwards: the first count slots are set up and the rest removed
HRESULT SetUpCameras(int count) {
    HRESULT hr = MFStartup(MF_VERSION, MFSTARTUP_LITE);
    if (FAILED(hr)) return hr;
    int done = 0;
    for (int slot = 0; slot < kSlots && SUCCEEDED(hr); slot++) {
        if (slot < count) {
            hr = RegisterClsid(slot);
            if (SUCCEEDED(hr)) hr = AddCamera(slot);
            if (SUCCEEDED(hr)) done = slot + 1;
        } else {
            RemoveCamera(slot);
            UnregisterClsid(slot);
        }
    }
    if (FAILED(hr)) {
        // Windows wouldn't take one (camera access blocked, Frame Server off): none stay, so Telescope uses UnityCapture for all
        for (int slot = 0; slot <= done && slot < kSlots; slot++) {
            RemoveCamera(slot);
            UnregisterClsid(slot);
        }
    }
    MFShutdown();
    return hr;
}

}  // namespace

const GUID& SlotClsid(int slot) {
    return kClsids[slot];
}

}  // namespace telescope

using namespace telescope;

BOOL APIENTRY DllMain(HMODULE module, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        g_module = module;
        DisableThreadLibraryCalls(module);
    }
    return TRUE;
}

STDAPI DllGetClassObject(REFCLSID clsid, REFIID riid, void** object) {
    if (!object) return E_POINTER;
    *object = nullptr;
    for (int slot = 0; slot < kSlots; slot++) {
        if (IsEqualCLSID(clsid, kClsids[slot])) {
            ComPtr<Factory> factory = Microsoft::WRL::Make<Factory>(slot);
            return factory ? factory.CopyTo(riid, object) : E_OUTOFMEMORY;
        }
    }
    return CLASS_E_CLASSNOTAVAILABLE;
}

STDAPI DllCanUnloadNow() {
    return g_objects == 0 ? S_OK : S_FALSE;
}

// regsvr32 TelescopeCamera.dll: just the first camera
STDAPI DllRegisterServer() {
    return SetUpCameras(1);
}

// regsvr32 /u: every camera this DLL added
STDAPI DllUnregisterServer() {
    return SetUpCameras(0);
}

// regsvr32 /n /i:<count>: that many cameras, removing any past it
STDAPI DllInstall(BOOL install, PCWSTR commandLine) {
    if (!install) return SetUpCameras(0);
    int count = commandLine && *commandLine ? _wtoi(commandLine) : 1;
    if (count < 1 || count > kSlots) return E_INVALIDARG;
    return SetUpCameras(count);
}
