#include "camera.h"

#include <sddl.h>
#include <stdio.h>
#include <string.h>

namespace telescope {

namespace {

constexpr ULONGLONG kStaleMs = 3000;  // Telescope sends at least once a second, so this long without a frame means it's gone
// The idle colour: the app's background (#0f1216) in BT.601 limited range
constexpr BYTE kIdleY = 31, kIdleU = 130, kIdleV = 126;
// Full access for the system, Frame Server's account and admins; read and write for people signed in at the computer
constexpr wchar_t kSecurity[] = L"D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GRGW;;;IU)S:(ML;;NW;;;ME)";

}  // namespace

void FrameChannel::Open(int slot, UINT32 width, UINT32 height, UINT32 fps) {
    Close();
    wchar_t name[64];
    swprintf_s(name, L"%s%d", kSharedName, slot);
    PSECURITY_DESCRIPTOR sd = nullptr;
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(kSecurity, SDDL_REVISION_1, &sd, nullptr)) {
        Log(L"slot %d: no security descriptor (%lu)", slot, GetLastError());
        return;
    }
    SECURITY_ATTRIBUTES sa = {sizeof(sa), sd, FALSE};
    m_map = CreateFileMappingW(INVALID_HANDLE_VALUE, &sa, PAGE_READWRITE, 0, kDataOffset + kDataCapacity, name);
    DWORD error = GetLastError();
    LocalFree(sd);
    if (!m_map) {
        Log(L"slot %d: can't make %s (%lu), showing the idle picture", slot, name, error);
        return;
    }
    m_header = static_cast<SharedHeader*>(MapViewOfFile(m_map, FILE_MAP_ALL_ACCESS, 0, 0, kDataOffset + kDataCapacity));
    if (!m_header) {
        Log(L"slot %d: can't map %s (%lu)", slot, name, GetLastError());
        CloseHandle(m_map);
        m_map = nullptr;
        return;
    }
    m_header->magic = kMagic;
    m_header->version = kVersion;
    m_header->data_offset = kDataOffset;
    m_header->data_capacity = kDataCapacity;
    m_header->consumer_width = width;
    m_header->consumer_height = height;
    m_header->consumer_fps = fps;
    MemoryBarrier();
    m_header->consumer_state = 1;
    m_counter = m_header->frame_counter;  // a frame left from an earlier reader isn't new
    m_have = false;
}

void FrameChannel::Close() {
    if (m_header) {
        m_header->consumer_state = 0;
        UnmapViewOfFile(m_header);
        m_header = nullptr;
    }
    if (m_map) {
        CloseHandle(m_map);
        m_map = nullptr;
    }
    m_have = false;
}

bool FrameChannel::ReadNewest(UINT32 width, UINT32 height) {
    if (!m_header) return false;
    volatile SharedHeader* header = m_header;
    UINT64 before = header->frame_counter;
    if ((before & 1) || before == m_counter) return false;
    // Telescope's memory is only trusted as far as it fits what this reader expects
    if (header->frame_format != kFormatNV12 || header->frame_width != width || header->frame_height != height) {
        return false;
    }
    size_t bytes = static_cast<size_t>(width) * height * 3 / 2;
    if (bytes > kDataCapacity) return false;
    m_frame.resize(bytes);
    MemoryBarrier();
    memcpy(m_frame.data(), reinterpret_cast<BYTE*>(m_header) + kDataOffset, bytes);
    MemoryBarrier();
    if (header->frame_counter != before) return false;  // written over while being copied
    m_counter = before;
    m_have = true;
    m_changedAt = GetTickCount64();
    return true;
}

void FrameChannel::Fill(BYTE* dst, LONG pitch, UINT32 width, UINT32 height) {
    ReadNewest(width, height);
    if (m_header) InterlockedIncrement64(reinterpret_cast<volatile LONG64*>(&m_header->consumer_heartbeat));
    BYTE* uv = dst + static_cast<size_t>(pitch) * height;
    if (m_have && m_frame.size() == static_cast<size_t>(width) * height * 3 / 2 &&
        GetTickCount64() - m_changedAt < kStaleMs) {
        const BYTE* src = m_frame.data();
        for (UINT32 row = 0; row < height; row++) memcpy(dst + row * static_cast<size_t>(pitch), src + row * width, width);
        src += static_cast<size_t>(width) * height;
        for (UINT32 row = 0; row < height / 2; row++) memcpy(uv + row * static_cast<size_t>(pitch), src + row * width, width);
        return;
    }
    for (UINT32 row = 0; row < height; row++) memset(dst + row * static_cast<size_t>(pitch), kIdleY, width);
    for (UINT32 row = 0; row < height / 2; row++) {
        BYTE* line = uv + row * static_cast<size_t>(pitch);
        for (UINT32 x = 0; x + 1 < width; x += 2) {
            line[x] = kIdleU;
            line[x + 1] = kIdleV;
        }
    }
}

void Log(const wchar_t* format, ...) {
    wchar_t text[512] = L"TelescopeCamera: ";
    size_t used = wcslen(text);
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(text + used, ARRAYSIZE(text) - used - 2, _TRUNCATE, format, args);
    va_end(args);
    wcscat_s(text, L"\n");
    OutputDebugStringW(text);
}

}  // namespace telescope
