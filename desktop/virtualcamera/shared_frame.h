// The shared memory the camera reads its frames from. desktop/telescope/vcam.py writes the same layout.
#pragma once
#include <stdint.h>

namespace telescope {

// The camera creates "Global\TelescopeCamera<slot>" when an app starts reading it and closes it when the app stops,
// so Telescope can tell the camera is in use from it being there.
constexpr wchar_t kSharedName[] = L"Global\\TelescopeCamera";
constexpr uint32_t kMagic = 0x4D435354;  // "TSCM"
constexpr uint32_t kVersion = 1;
constexpr uint32_t kFormatNV12 = 1;
constexpr uint32_t kMaxWidth = 3840;
constexpr uint32_t kMaxHeight = 3840;
constexpr uint32_t kDataOffset = 64;
constexpr uint32_t kDataCapacity = 3840 * 2160 * 3 / 2;  // the biggest size the camera offers, as NV12

#pragma pack(push, 1)
struct SharedHeader {
    // Written by the camera
    uint32_t magic;
    uint32_t version;
    uint32_t data_offset;
    uint32_t data_capacity;
    uint32_t consumer_state;      // 1 while an app reads the camera
    uint32_t consumer_width;      // the size the app picked, which Telescope sends at
    uint32_t consumer_height;
    uint32_t consumer_fps;
    uint64_t consumer_heartbeat;  // goes up with every frame an app is handed
    // Written by Telescope
    uint64_t frame_counter;       // odd while a frame is being written
    uint32_t frame_width;
    uint32_t frame_height;
    uint32_t frame_format;
    uint32_t reserved;
};
#pragma pack(pop)

static_assert(sizeof(SharedHeader) == kDataOffset, "the header is 64 bytes");

}  // namespace telescope
