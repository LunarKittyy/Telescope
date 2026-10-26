#include "camera.h"

#include <algorithm>
#include <chrono>
#include <propvarutil.h>

namespace telescope {

namespace {

struct Mode {
    UINT32 width, height, fps;
};

// Telescope fits its picture to whichever one an app picks. No portrait ones: some apps (the Windows Camera app) pick
// one of those if offered, and a webcam is expected to be landscape.
constexpr Mode kModes[] = {
    {1920, 1080, 30}, {1920, 1080, 60}, {1280, 720, 30}, {1280, 720, 60}, {3840, 2160, 30},
    {2560, 1440, 30}, {960, 540, 30},   {640, 480, 30},  {640, 360, 30},
};

HRESULT MakeType(const Mode& mode, IMFMediaType** out) {
    ComPtr<IMFMediaType> type;
    HRESULT hr = MFCreateMediaType(&type);
    if (FAILED(hr)) return hr;
    type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_NV12);
    type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    type->SetUINT32(MF_MT_ALL_SAMPLES_INDEPENDENT, TRUE);
    type->SetUINT32(MF_MT_FIXED_SIZE_SAMPLES, TRUE);
    type->SetUINT32(MF_MT_DEFAULT_STRIDE, mode.width);
    type->SetUINT32(MF_MT_SAMPLE_SIZE, mode.width * mode.height * 3 / 2);
    MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, mode.width, mode.height);
    MFSetAttributeRatio(type.Get(), MF_MT_FRAME_RATE, mode.fps, 1);
    MFSetAttributeRatio(type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1);
    type->SetUINT32(MF_MT_AVG_BITRATE, mode.width * mode.height * 12 * mode.fps);
    *out = type.Detach();
    return S_OK;
}

HRESULT SetStreamAttributes(IMFAttributes* attrs) {
    HRESULT hr = attrs->SetGUID(MF_DEVICESTREAM_STREAM_CATEGORY, PINNAME_VIDEO_CAPTURE);
    if (SUCCEEDED(hr)) hr = attrs->SetUINT32(MF_DEVICESTREAM_STREAM_ID, 0);
    if (SUCCEEDED(hr)) hr = attrs->SetUINT32(MF_DEVICESTREAM_FRAMESERVER_SHARED, 1);
    if (SUCCEEDED(hr)) hr = attrs->SetUINT32(MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color);
    return hr;
}

}  // namespace

// ── MediaStream ───────────────────────────────────────────────────────────

MediaStream::~MediaStream() {
    Halt();
}

HRESULT MediaStream::RuntimeClassInitialize(MediaSource* source, int slot) {
    std::lock_guard<std::mutex> lock(m_lock);
    m_source = source;
    m_slot = slot;

    constexpr DWORD count = ARRAYSIZE(kModes);
    IMFMediaType* types[count] = {};
    HRESULT hr = S_OK;
    for (DWORD i = 0; i < count && SUCCEEDED(hr); i++) hr = MakeType(kModes[i], &types[i]);
    if (SUCCEEDED(hr)) hr = MFCreateAttributes(&m_attributes, 4);
    if (SUCCEEDED(hr)) hr = SetStreamAttributes(m_attributes.Get());
    if (SUCCEEDED(hr)) hr = MFCreateEventQueue(&m_queue);
    if (SUCCEEDED(hr)) hr = MFCreateStreamDescriptor(0, count, types, &m_descriptor);
    ComPtr<IMFMediaTypeHandler> handler;
    if (SUCCEEDED(hr)) hr = m_descriptor->GetMediaTypeHandler(&handler);
    if (SUCCEEDED(hr)) hr = handler->SetCurrentMediaType(types[0]);
    if (SUCCEEDED(hr)) hr = SetStreamAttributes(m_descriptor.Get());
    for (auto* type : types) {
        if (type) type->Release();
    }
    return hr;
}

HRESULT MediaStream::CheckShutdown() const {
    return m_shutdown ? MF_E_SHUTDOWN : (m_queue ? S_OK : E_UNEXPECTED);
}

IFACEMETHODIMP MediaStream::BeginGetEvent(IMFAsyncCallback* callback, IUnknown* state) {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_queue->BeginGetEvent(callback, state);
}

IFACEMETHODIMP MediaStream::EndGetEvent(IMFAsyncResult* result, IMFMediaEvent** event) {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_queue->EndGetEvent(result, event);
}

IFACEMETHODIMP MediaStream::GetEvent(DWORD flags, IMFMediaEvent** event) {
    ComPtr<IMFMediaEventQueue> queue;
    {
        std::lock_guard<std::mutex> lock(m_lock);  // not held while waiting: GetEvent can block
        HRESULT hr = CheckShutdown();
        if (FAILED(hr)) return hr;
        queue = m_queue;
    }
    return queue->GetEvent(flags, event);
}

IFACEMETHODIMP MediaStream::QueueEvent(MediaEventType type, REFGUID extended, HRESULT status, const PROPVARIANT* v) {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_queue->QueueEventParamVar(type, extended, status, v);
}

IFACEMETHODIMP MediaStream::GetMediaSource(IMFMediaSource** source) {
    if (!source) return E_POINTER;
    *source = nullptr;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_source.CopyTo(source);
}

IFACEMETHODIMP MediaStream::GetStreamDescriptor(IMFStreamDescriptor** descriptor) {
    if (!descriptor) return E_POINTER;
    *descriptor = nullptr;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_descriptor.CopyTo(descriptor);
}

IFACEMETHODIMP MediaStream::RequestSample(IUnknown* token) {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr)) return hr;
    if (m_state != MF_STREAM_STATE_RUNNING) return MF_E_INVALIDREQUEST;
    m_tokens.emplace_back(token);
    m_wake.notify_all();
    return S_OK;
}

IFACEMETHODIMP MediaStream::SetStreamState(MF_STREAM_STATE state) {
    std::unique_lock<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr) || state == m_state) return hr;
    switch (state) {
    case MF_STREAM_STATE_PAUSED:
        if (m_state != MF_STREAM_STATE_RUNNING) return MF_E_INVALID_STATE_TRANSITION;
        m_state = state;
        return S_OK;
    case MF_STREAM_STATE_RUNNING:
        lock.unlock();
        return Run(nullptr);
    case MF_STREAM_STATE_STOPPED:
        lock.unlock();
        Halt();
        return S_OK;
    default:
        return MF_E_INVALID_STATE_TRANSITION;
    }
}

IFACEMETHODIMP MediaStream::GetStreamState(MF_STREAM_STATE* state) {
    if (!state) return E_POINTER;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (SUCCEEDED(hr)) *state = m_state;
    return hr;
}

HRESULT MediaStream::Start(IMFMediaType* type) {
    if (!type) return E_INVALIDARG;
    HRESULT hr = Run(type);
    if (SUCCEEDED(hr)) hr = QueueEvent(MEStreamStarted, GUID_NULL, S_OK, nullptr);
    return hr;
}

HRESULT MediaStream::Stop(bool sendEvent) {
    Halt();
    return sendEvent ? QueueEvent(MEStreamStopped, GUID_NULL, S_OK, nullptr) : S_OK;
}

HRESULT MediaStream::Run(IMFMediaType* type) {
    Halt();  // a new media type starts over
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr)) return hr;
    if (type) m_type = type;
    if (!m_type) return MF_E_NOT_INITIALIZED;
    UINT32 num = 30, den = 1;
    hr = MFGetAttributeSize(m_type.Get(), MF_MT_FRAME_SIZE, &m_width, &m_height);
    if (FAILED(hr)) return hr;
    if (SUCCEEDED(MFGetAttributeRatio(m_type.Get(), MF_MT_FRAME_RATE, &num, &den)) && den) m_fps = std::max(1u, num / den);
    if (!m_allocator) {
        hr = MFCreateVideoSampleAllocatorEx(IID_PPV_ARGS(&m_allocator));
        if (FAILED(hr)) return hr;
    }
    hr = m_allocator->InitializeSampleAllocator(10, m_type.Get());
    if (FAILED(hr)) return hr;
    Log(L"slot %d streams %ux%u at %u fps", m_slot, m_width, m_height, m_fps);
    m_channel.Open(m_slot, m_width, m_height, m_fps);
    m_stopWorker = false;
    m_state = MF_STREAM_STATE_RUNNING;
    m_worker = std::thread(&MediaStream::Deliver, this);
    return S_OK;
}

void MediaStream::Halt() {
    std::thread worker;
    {
        std::lock_guard<std::mutex> lock(m_lock);
        m_state = MF_STREAM_STATE_STOPPED;
        m_stopWorker = true;
        m_tokens.clear();
        worker = std::move(m_worker);
        m_wake.notify_all();
    }
    if (worker.joinable()) worker.join();
    std::lock_guard<std::mutex> lock(m_lock);
    m_channel.Close();
}

void MediaStream::Shutdown() {
    Halt();
    std::lock_guard<std::mutex> lock(m_lock);
    m_shutdown = true;
    if (m_queue) m_queue->Shutdown();
    m_queue.Reset();
    m_source.Reset();
    m_allocator.Reset();
}

HRESULT MediaStream::SetAllocator(IMFVideoSampleAllocator* allocator) {
    std::lock_guard<std::mutex> lock(m_lock);
    if (m_state == MF_STREAM_STATE_RUNNING) return MF_E_INVALIDREQUEST;
    m_allocator = allocator;
    return S_OK;
}

// Hands out a frame for each request, no faster than the media type's frame rate.
void MediaStream::Deliver() {
    using clock = std::chrono::steady_clock;
    auto next = clock::now();
    std::unique_lock<std::mutex> lock(m_lock);
    while (!m_stopWorker) {
        m_wake.wait(lock, [this] { return m_stopWorker || !m_tokens.empty(); });
        if (m_stopWorker) break;
        if (m_wake.wait_until(lock, next, [this] { return m_stopWorker; })) break;
        if (m_tokens.empty() || m_state != MF_STREAM_STATE_RUNNING) continue;
        ComPtr<IUnknown> token = m_tokens.front();
        m_tokens.pop_front();
        HRESULT hr = MakeSample(token.Get());
        if (FAILED(hr)) {
            Log(L"slot %d: making a frame failed (0x%08x)", m_slot, hr);
            if (m_queue) m_queue->QueueEventParamVar(MEError, GUID_NULL, hr, nullptr);
            break;
        }
        auto period = std::chrono::nanoseconds(1000000000LL / m_fps);
        auto now = clock::now();
        next = (next + period < now) ? now : next + period;  // a late frame doesn't make the next ones rush
    }
}

HRESULT MediaStream::MakeSample(IUnknown* token) {
    ComPtr<IMFSample> sample;
    ComPtr<IMFMediaBuffer> buffer;
    HRESULT hr = m_allocator->AllocateSample(&sample);
    if (SUCCEEDED(hr)) hr = sample->GetBufferByIndex(0, &buffer);
    if (FAILED(hr)) return hr;
    ComPtr<IMF2DBuffer2> buffer2d;
    if (SUCCEEDED(buffer.As(&buffer2d))) {
        BYTE* scan0 = nullptr;
        BYTE* start = nullptr;
        LONG pitch = 0;
        DWORD length = 0;
        hr = buffer2d->Lock2DSize(MF2DBuffer_LockFlags_Write, &scan0, &pitch, &start, &length);
        if (FAILED(hr)) return hr;
        if (pitch > 0 && static_cast<DWORD>(pitch) * m_height * 3 / 2 <= length) {
            m_channel.Fill(scan0, pitch, m_width, m_height);
        }
        buffer2d->Unlock2D();
    } else {
        BYTE* data = nullptr;
        DWORD max = 0;
        hr = buffer->Lock(&data, &max, nullptr);
        if (FAILED(hr)) return hr;
        if (max >= m_width * m_height * 3 / 2) m_channel.Fill(data, m_width, m_width, m_height);
        buffer->Unlock();
        buffer->SetCurrentLength(m_width * m_height * 3 / 2);
    }
    sample->SetSampleTime(MFGetSystemTime());
    sample->SetSampleDuration(10000000LL / m_fps);
    if (token) sample->SetUnknown(MFSampleExtension_Token, token);
    return m_queue->QueueEventParamUnk(MEMediaSample, GUID_NULL, S_OK, sample.Get());
}

// ── MediaSource ───────────────────────────────────────────────────────────

HRESULT MediaSource::RuntimeClassInitialize(int slot, IMFAttributes* activateAttributes) {
    std::lock_guard<std::mutex> lock(m_lock);
    if (m_state != State::Invalid) return MF_E_ALREADY_INITIALIZED;

    HRESULT hr = MFCreateAttributes(&m_attributes, 4);
    if (SUCCEEDED(hr) && activateAttributes) hr = activateAttributes->CopyAllItems(m_attributes.Get());

    // Frame Server wants at least the legacy profile; this one allows every media type.
    ComPtr<IMFSensorProfileCollection> profiles;
    ComPtr<IMFSensorProfile> profile;
    if (SUCCEEDED(hr)) hr = MFCreateSensorProfileCollection(&profiles);
    if (SUCCEEDED(hr)) hr = MFCreateSensorProfile(KSCAMERAPROFILE_Legacy, 0, nullptr, &profile);
    if (SUCCEEDED(hr)) hr = profile->AddProfileFilter(0, L"((RES==;FRT==;SUT==))");
    if (SUCCEEDED(hr)) hr = profiles->AddProfile(profile.Get());
    if (SUCCEEDED(hr)) hr = m_attributes->SetUnknown(MF_DEVICEMFT_SENSORPROFILE_COLLECTION, profiles.Get());

    if (SUCCEEDED(hr)) hr = MFCreateEventQueue(&m_queue);
    if (SUCCEEDED(hr)) hr = Microsoft::WRL::MakeAndInitialize<MediaStream>(&m_stream, this, slot);
    IMFStreamDescriptor* descriptors[] = {m_stream ? m_stream->Descriptor() : nullptr};
    if (SUCCEEDED(hr)) hr = MFCreatePresentationDescriptor(1, descriptors, &m_descriptor);
    if (SUCCEEDED(hr)) hr = m_descriptor->SelectStream(0);
    if (SUCCEEDED(hr)) m_state = State::Stopped;
    return hr;
}

HRESULT MediaSource::CheckShutdown() const {
    if (m_state == State::Shutdown) return MF_E_SHUTDOWN;
    return (m_queue && m_stream) ? S_OK : E_UNEXPECTED;
}

IFACEMETHODIMP MediaSource::BeginGetEvent(IMFAsyncCallback* callback, IUnknown* state) {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_queue->BeginGetEvent(callback, state);
}

IFACEMETHODIMP MediaSource::EndGetEvent(IMFAsyncResult* result, IMFMediaEvent** event) {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_queue->EndGetEvent(result, event);
}

IFACEMETHODIMP MediaSource::GetEvent(DWORD flags, IMFMediaEvent** event) {
    ComPtr<IMFMediaEventQueue> queue;
    {
        std::lock_guard<std::mutex> lock(m_lock);
        HRESULT hr = CheckShutdown();
        if (FAILED(hr)) return hr;
        queue = m_queue;
    }
    return queue->GetEvent(flags, event);
}

IFACEMETHODIMP MediaSource::QueueEvent(MediaEventType type, REFGUID extended, HRESULT status, const PROPVARIANT* v) {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_queue->QueueEventParamVar(type, extended, status, v);
}

IFACEMETHODIMP MediaSource::CreatePresentationDescriptor(IMFPresentationDescriptor** descriptor) {
    if (!descriptor) return E_POINTER;
    *descriptor = nullptr;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_descriptor->Clone(descriptor);
}

IFACEMETHODIMP MediaSource::GetCharacteristics(DWORD* characteristics) {
    if (!characteristics) return E_POINTER;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    *characteristics = SUCCEEDED(hr) ? MFMEDIASOURCE_IS_LIVE : 0;
    return hr;
}

IFACEMETHODIMP MediaSource::Pause() {
    return MF_E_INVALID_STATE_TRANSITION;
}

IFACEMETHODIMP MediaSource::Shutdown() {
    ComPtr<MediaStream> stream;
    {
        std::lock_guard<std::mutex> lock(m_lock);
        m_state = State::Shutdown;
        if (m_queue) m_queue->Shutdown();
        m_queue.Reset();
        m_descriptor.Reset();
        m_attributes.Reset();
        stream = std::move(m_stream);
    }
    if (stream) stream->Shutdown();
    return S_OK;
}

IFACEMETHODIMP MediaSource::Start(IMFPresentationDescriptor* descriptor, const GUID* timeFormat,
                                  const PROPVARIANT* startPosition) {
    if (!descriptor || !startPosition) return E_INVALIDARG;
    if (timeFormat && *timeFormat != GUID_NULL) return MF_E_UNSUPPORTED_TIME_FORMAT;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr)) return hr;
    if (m_state == State::Invalid) return MF_E_INVALID_STATE_TRANSITION;

    DWORD count = 0;
    hr = descriptor->GetStreamDescriptorCount(&count);
    if (FAILED(hr)) return hr;
    if (count != 1) return E_INVALIDARG;
    BOOL selected = FALSE;
    ComPtr<IMFStreamDescriptor> streamDescriptor;
    hr = descriptor->GetStreamDescriptorByIndex(0, &selected, &streamDescriptor);
    if (FAILED(hr)) return hr;
    BOOL wasSelected = FALSE;
    ComPtr<IMFStreamDescriptor> ours;
    m_descriptor->GetStreamDescriptorByIndex(0, &wasSelected, &ours);
    bool wasRunning = m_state == State::Started && wasSelected;

    if (selected) {
        ComPtr<IMFMediaTypeHandler> handler;
        ComPtr<IMFMediaType> type;
        hr = streamDescriptor->GetMediaTypeHandler(&handler);
        if (SUCCEEDED(hr)) hr = handler->GetCurrentMediaType(&type);
        if (FAILED(hr)) return hr;
        m_descriptor->SelectStream(0);
        ComPtr<IUnknown> unknown;
        m_stream.As(&unknown);
        hr = m_queue->QueueEventParamUnk(wasRunning ? MEUpdatedStream : MENewStream, GUID_NULL, S_OK, unknown.Get());
        if (SUCCEEDED(hr)) hr = m_stream->Start(type.Get());
        if (FAILED(hr)) return hr;
    } else if (wasSelected) {
        m_descriptor->DeselectStream(0);
        m_stream->Stop(false);
    }

    PROPVARIANT time;
    InitPropVariantFromInt64(MFGetSystemTime(), &time);
    hr = m_queue->QueueEventParamVar(MESourceStarted, GUID_NULL, S_OK, &time);
    PropVariantClear(&time);
    if (SUCCEEDED(hr)) m_state = State::Started;
    return hr;
}

IFACEMETHODIMP MediaSource::Stop() {
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr)) return hr;
    if (m_state != State::Started) return MF_E_INVALID_STATE_TRANSITION;
    m_state = State::Stopped;
    m_stream->Stop(true);
    m_descriptor->DeselectStream(0);
    PROPVARIANT time;
    InitPropVariantFromInt64(MFGetSystemTime(), &time);
    hr = m_queue->QueueEventParamVar(MESourceStopped, GUID_NULL, S_OK, &time);
    PropVariantClear(&time);
    return hr;
}

IFACEMETHODIMP MediaSource::GetSourceAttributes(IMFAttributes** attributes) {
    if (!attributes) return E_POINTER;
    *attributes = nullptr;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    return FAILED(hr) ? hr : m_attributes.CopyTo(attributes);  // Frame Server expects the store itself, not a copy
}

IFACEMETHODIMP MediaSource::GetStreamAttributes(DWORD streamId, IMFAttributes** attributes) {
    if (!attributes) return E_POINTER;
    *attributes = nullptr;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr)) return hr;
    if (streamId != 0) return MF_E_NOT_FOUND;
    IMFAttributes* attrs = m_stream->Attributes();
    attrs->AddRef();
    *attributes = attrs;
    return S_OK;
}

IFACEMETHODIMP MediaSource::SetD3DManager(IUnknown*) {
    return E_NOTIMPL;  // frames are written in system memory
}

IFACEMETHODIMP MediaSource::GetService(REFGUID, REFIID, LPVOID* object) {
    if (!object) return E_POINTER;
    *object = nullptr;
    return MF_E_UNSUPPORTED_SERVICE;
}

// No camera controls: answer like a driver without them
IFACEMETHODIMP MediaSource::KsProperty(PKSPROPERTY, ULONG, LPVOID, ULONG, ULONG*) {
    return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
}

IFACEMETHODIMP MediaSource::KsMethod(PKSMETHOD, ULONG, LPVOID, ULONG, ULONG*) {
    return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
}

IFACEMETHODIMP MediaSource::KsEvent(PKSEVENT, ULONG, LPVOID, ULONG, ULONG*) {
    return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
}

IFACEMETHODIMP MediaSource::SetDefaultAllocator(DWORD streamId, IUnknown* allocator) {
    if (!allocator) return E_POINTER;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr)) return hr;
    if (streamId != 0) return MF_E_NOT_FOUND;
    ComPtr<IMFVideoSampleAllocator> sampleAllocator;
    hr = allocator->QueryInterface(IID_PPV_ARGS(&sampleAllocator));
    return FAILED(hr) ? hr : m_stream->SetAllocator(sampleAllocator.Get());
}

IFACEMETHODIMP MediaSource::GetAllocatorUsage(DWORD streamId, DWORD* inputStreamId, MFSampleAllocatorUsage* usage) {
    if (!inputStreamId || !usage) return E_POINTER;
    std::lock_guard<std::mutex> lock(m_lock);
    HRESULT hr = CheckShutdown();
    if (FAILED(hr)) return hr;
    if (streamId != 0) return MF_E_NOT_FOUND;
    *inputStreamId = streamId;
    *usage = MFSampleAllocatorUsage_UsesProvidedAllocator;
    return S_OK;
}

// ── SourceActivate ────────────────────────────────────────────────────────

HRESULT SourceActivate::RuntimeClassInitialize(int slot) {
    m_slot = slot;
    HRESULT hr = MFCreateAttributes(&m_attrs, 2);
    if (SUCCEEDED(hr)) hr = m_attrs->SetUINT32(MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1);
    return hr;
}

IFACEMETHODIMP SourceActivate::ActivateObject(REFIID riid, void** object) {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (!m_source) {
        ComPtr<MediaSource> source;
        HRESULT hr = Microsoft::WRL::MakeAndInitialize<MediaSource>(&source, m_slot, m_attrs.Get());
        if (FAILED(hr)) {
            Log(L"slot %d: the media source didn't start (0x%08x)", m_slot, hr);
            return hr;
        }
        m_source = source;
    }
    return m_source.CopyTo(riid, object);
}

IFACEMETHODIMP SourceActivate::ShutdownObject() {
    if (m_source) m_source->Shutdown();
    m_source.Reset();
    return S_OK;
}

IFACEMETHODIMP SourceActivate::DetachObject() {
    m_source.Reset();
    return S_OK;
}

}  // namespace telescope
