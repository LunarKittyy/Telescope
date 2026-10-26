// Telescope's Media Foundation virtual camera: a media source Windows' Frame Server loads for every app.
#pragma once

#include <windows.h>
#include <unknwn.h>
#include <ole2.h>
#include <initguid.h>
#include <ks.h>
#include <ksproxy.h>
#include <ksmedia.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mferror.h>
#include <mfvirtualcamera.h>
#include <wrl/client.h>
#include <wrl/implements.h>

#include <atomic>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <thread>
#include <vector>

#include "shared_frame.h"

namespace telescope {

using Microsoft::WRL::ChainInterfaces;
using Microsoft::WRL::ClassicCom;
using Microsoft::WRL::ComPtr;
using Microsoft::WRL::RuntimeClass;
using Microsoft::WRL::RuntimeClassFlags;

constexpr int kSlots = 4;  // matches vcam.MAX_SLOTS
// Registered with each camera; Telescope offers to install its own DLL over one with a lower number (MF_VERSION)
constexpr DWORD kCameraVersion = 2;

extern std::atomic<long> g_objects;  // live objects and server locks, for DllCanUnloadNow
extern HMODULE g_module;

const GUID& SlotClsid(int slot);
void Log(const wchar_t* format, ...);

// Counts an object for DllCanUnloadNow while it lives.
struct ModuleRef {
    ModuleRef() { ++g_objects; }
    ~ModuleRef() { --g_objects; }
};

// Reads Telescope's frames from the shared memory, and makes it while an app reads the camera.
class FrameChannel {
public:
    ~FrameChannel() { Close(); }
    void Open(int slot, UINT32 width, UINT32 height, UINT32 fps);
    void Close();
    // Writes the newest frame as NV12 into dst, or the idle colour while Telescope sends nothing.
    void Fill(BYTE* dst, LONG pitch, UINT32 width, UINT32 height);

private:
    bool ReadNewest(UINT32 width, UINT32 height);

    HANDLE m_map = nullptr;
    SharedHeader* m_header = nullptr;
    std::vector<BYTE> m_frame;
    UINT64 m_counter = 0;
    ULONGLONG m_changedAt = 0;
    bool m_have = false;
};

class MediaSource;

class MediaStream : public RuntimeClass<RuntimeClassFlags<ClassicCom>,
                                        ChainInterfaces<IMFMediaStream2, IMFMediaStream, IMFMediaEventGenerator>> {
public:
    MediaStream() = default;
    ~MediaStream();
    HRESULT RuntimeClassInitialize(MediaSource* source, int slot);
    HRESULT Start(IMFMediaType* type);
    HRESULT Stop(bool sendEvent);
    void Shutdown();
    HRESULT SetAllocator(IMFVideoSampleAllocator* allocator);
    IMFStreamDescriptor* Descriptor() { return m_descriptor.Get(); }
    IMFAttributes* Attributes() { return m_attributes.Get(); }

    // IMFMediaEventGenerator
    IFACEMETHODIMP BeginGetEvent(IMFAsyncCallback* callback, IUnknown* state) override;
    IFACEMETHODIMP EndGetEvent(IMFAsyncResult* result, IMFMediaEvent** event) override;
    IFACEMETHODIMP GetEvent(DWORD flags, IMFMediaEvent** event) override;
    IFACEMETHODIMP QueueEvent(MediaEventType type, REFGUID extended, HRESULT status, const PROPVARIANT* v) override;
    // IMFMediaStream
    IFACEMETHODIMP GetMediaSource(IMFMediaSource** source) override;
    IFACEMETHODIMP GetStreamDescriptor(IMFStreamDescriptor** descriptor) override;
    IFACEMETHODIMP RequestSample(IUnknown* token) override;
    // IMFMediaStream2
    IFACEMETHODIMP SetStreamState(MF_STREAM_STATE state) override;
    IFACEMETHODIMP GetStreamState(MF_STREAM_STATE* state) override;

private:
    HRESULT CheckShutdown() const;
    HRESULT Run(IMFMediaType* type);
    void Halt();
    void Deliver();
    HRESULT MakeSample(IUnknown* token);

    std::mutex m_lock;
    std::condition_variable m_wake;
    std::thread m_worker;
    bool m_stopWorker = false;
    bool m_shutdown = false;
    int m_slot = 0;
    MF_STREAM_STATE m_state = MF_STREAM_STATE_STOPPED;
    ComPtr<IMFMediaSource> m_source;
    ComPtr<IMFMediaEventQueue> m_queue;
    ComPtr<IMFStreamDescriptor> m_descriptor;
    ComPtr<IMFAttributes> m_attributes;
    ComPtr<IMFMediaType> m_type;
    ComPtr<IMFVideoSampleAllocator> m_allocator;
    std::deque<ComPtr<IUnknown>> m_tokens;
    UINT32 m_width = 0, m_height = 0, m_fps = 30;
    FrameChannel m_channel;
    ModuleRef m_ref;
};

class MediaSource : public RuntimeClass<RuntimeClassFlags<ClassicCom>,
                                        ChainInterfaces<IMFMediaSourceEx, IMFMediaSource, IMFMediaEventGenerator>,
                                        IMFGetService, IKsControl, IMFSampleAllocatorControl> {
public:
    HRESULT RuntimeClassInitialize(int slot, IMFAttributes* activateAttributes);

    // IMFMediaEventGenerator
    IFACEMETHODIMP BeginGetEvent(IMFAsyncCallback* callback, IUnknown* state) override;
    IFACEMETHODIMP EndGetEvent(IMFAsyncResult* result, IMFMediaEvent** event) override;
    IFACEMETHODIMP GetEvent(DWORD flags, IMFMediaEvent** event) override;
    IFACEMETHODIMP QueueEvent(MediaEventType type, REFGUID extended, HRESULT status, const PROPVARIANT* v) override;
    // IMFMediaSource
    IFACEMETHODIMP CreatePresentationDescriptor(IMFPresentationDescriptor** descriptor) override;
    IFACEMETHODIMP GetCharacteristics(DWORD* characteristics) override;
    IFACEMETHODIMP Pause() override;
    IFACEMETHODIMP Shutdown() override;
    IFACEMETHODIMP Start(IMFPresentationDescriptor* descriptor, const GUID* timeFormat,
                         const PROPVARIANT* startPosition) override;
    IFACEMETHODIMP Stop() override;
    // IMFMediaSourceEx
    IFACEMETHODIMP GetSourceAttributes(IMFAttributes** attributes) override;
    IFACEMETHODIMP GetStreamAttributes(DWORD streamId, IMFAttributes** attributes) override;
    IFACEMETHODIMP SetD3DManager(IUnknown* manager) override;
    // IMFGetService
    IFACEMETHODIMP GetService(REFGUID service, REFIID riid, LPVOID* object) override;
    // IKsControl
    IFACEMETHODIMP KsProperty(PKSPROPERTY property, ULONG propertyLength, LPVOID data, ULONG dataLength,
                              ULONG* returned) override;
    IFACEMETHODIMP KsMethod(PKSMETHOD method, ULONG methodLength, LPVOID data, ULONG dataLength,
                            ULONG* returned) override;
    IFACEMETHODIMP KsEvent(PKSEVENT event, ULONG eventLength, LPVOID data, ULONG dataLength, ULONG* returned) override;
    // IMFSampleAllocatorControl
    IFACEMETHODIMP SetDefaultAllocator(DWORD streamId, IUnknown* allocator) override;
    IFACEMETHODIMP GetAllocatorUsage(DWORD streamId, DWORD* inputStreamId, MFSampleAllocatorUsage* usage) override;

private:
    enum class State { Invalid, Stopped, Started, Shutdown };
    HRESULT CheckShutdown() const;

    std::mutex m_lock;
    State m_state = State::Invalid;
    ComPtr<IMFMediaEventQueue> m_queue;
    ComPtr<IMFPresentationDescriptor> m_descriptor;
    ComPtr<IMFAttributes> m_attributes;
    ComPtr<MediaStream> m_stream;
    ModuleRef m_ref;
};

// What Frame Server creates from the registered CLSID; it makes the media source.
class SourceActivate : public RuntimeClass<RuntimeClassFlags<ClassicCom>, ChainInterfaces<IMFActivate, IMFAttributes>> {
public:
    HRESULT RuntimeClassInitialize(int slot);

    IFACEMETHODIMP ActivateObject(REFIID riid, void** object) override;
    IFACEMETHODIMP ShutdownObject() override;
    IFACEMETHODIMP DetachObject() override;

    // IMFAttributes, handed on to the attribute store
    IFACEMETHODIMP GetItem(REFGUID key, PROPVARIANT* v) override { return m_attrs->GetItem(key, v); }
    IFACEMETHODIMP GetItemType(REFGUID key, MF_ATTRIBUTE_TYPE* type) override { return m_attrs->GetItemType(key, type); }
    IFACEMETHODIMP CompareItem(REFGUID key, REFPROPVARIANT v, BOOL* result) override { return m_attrs->CompareItem(key, v, result); }
    IFACEMETHODIMP Compare(IMFAttributes* theirs, MF_ATTRIBUTES_MATCH_TYPE type, BOOL* result) override { return m_attrs->Compare(theirs, type, result); }
    IFACEMETHODIMP GetUINT32(REFGUID key, UINT32* v) override { return m_attrs->GetUINT32(key, v); }
    IFACEMETHODIMP GetUINT64(REFGUID key, UINT64* v) override { return m_attrs->GetUINT64(key, v); }
    IFACEMETHODIMP GetDouble(REFGUID key, double* v) override { return m_attrs->GetDouble(key, v); }
    IFACEMETHODIMP GetGUID(REFGUID key, GUID* v) override { return m_attrs->GetGUID(key, v); }
    IFACEMETHODIMP GetStringLength(REFGUID key, UINT32* length) override { return m_attrs->GetStringLength(key, length); }
    IFACEMETHODIMP GetString(REFGUID key, LPWSTR v, UINT32 size, UINT32* length) override { return m_attrs->GetString(key, v, size, length); }
    IFACEMETHODIMP GetAllocatedString(REFGUID key, LPWSTR* v, UINT32* length) override { return m_attrs->GetAllocatedString(key, v, length); }
    IFACEMETHODIMP GetBlobSize(REFGUID key, UINT32* size) override { return m_attrs->GetBlobSize(key, size); }
    IFACEMETHODIMP GetBlob(REFGUID key, UINT8* buf, UINT32 size, UINT32* blobSize) override { return m_attrs->GetBlob(key, buf, size, blobSize); }
    IFACEMETHODIMP GetAllocatedBlob(REFGUID key, UINT8** buf, UINT32* size) override { return m_attrs->GetAllocatedBlob(key, buf, size); }
    IFACEMETHODIMP GetUnknown(REFGUID key, REFIID riid, LPVOID* object) override { return m_attrs->GetUnknown(key, riid, object); }
    IFACEMETHODIMP SetItem(REFGUID key, REFPROPVARIANT v) override { return m_attrs->SetItem(key, v); }
    IFACEMETHODIMP DeleteItem(REFGUID key) override { return m_attrs->DeleteItem(key); }
    IFACEMETHODIMP DeleteAllItems() override { return m_attrs->DeleteAllItems(); }
    IFACEMETHODIMP SetUINT32(REFGUID key, UINT32 v) override { return m_attrs->SetUINT32(key, v); }
    IFACEMETHODIMP SetUINT64(REFGUID key, UINT64 v) override { return m_attrs->SetUINT64(key, v); }
    IFACEMETHODIMP SetDouble(REFGUID key, double v) override { return m_attrs->SetDouble(key, v); }
    IFACEMETHODIMP SetGUID(REFGUID key, REFGUID v) override { return m_attrs->SetGUID(key, v); }
    IFACEMETHODIMP SetString(REFGUID key, LPCWSTR v) override { return m_attrs->SetString(key, v); }
    IFACEMETHODIMP SetBlob(REFGUID key, const UINT8* buf, UINT32 size) override { return m_attrs->SetBlob(key, buf, size); }
    IFACEMETHODIMP SetUnknown(REFGUID key, IUnknown* v) override { return m_attrs->SetUnknown(key, v); }
    IFACEMETHODIMP LockStore() override { return m_attrs->LockStore(); }
    IFACEMETHODIMP UnlockStore() override { return m_attrs->UnlockStore(); }
    IFACEMETHODIMP GetCount(UINT32* count) override { return m_attrs->GetCount(count); }
    IFACEMETHODIMP GetItemByIndex(UINT32 index, GUID* key, PROPVARIANT* v) override { return m_attrs->GetItemByIndex(index, key, v); }
    IFACEMETHODIMP CopyAllItems(IMFAttributes* dest) override { return m_attrs->CopyAllItems(dest); }

private:
    int m_slot = 0;
    ComPtr<IMFAttributes> m_attrs;
    ComPtr<MediaSource> m_source;
    ModuleRef m_ref;
};

}  // namespace telescope
