// The camera's wait screen: scripts/write_camera_wait_screen.py renders it with vcam.py and it's built into the DLL.
#pragma once
#include <stddef.h>
#include <stdint.h>

#include <vector>

namespace telescope {

constexpr uint32_t kWaitScreenMagic = 0x53575354;  // "TSWS"
constexpr uint32_t kWaitScreenVersion = 1;
constexpr int kWaitScreenResource = 101;  // its RCDATA id

// The layout vcam.screen_layout() uses, in the project banner's units, and the pictures to place by it. Followed by
// the lens's Y and interleaved UV, then the name's mask.
#pragma pack(push, 1)
struct WaitScreenHeader {
    uint32_t magic;
    uint32_t version;
    uint32_t bg_y, bg_u, bg_v;
    uint32_t text_y, text_u, text_v;
    uint32_t width_share_milli, height_share_milli;
    uint32_t screen_width, screen_lens, screen_with_name, centre, name_top, name_px, name_min_px;
    uint32_t lens_size, lens_scale_milli;  // a square on the background, the lens at its centre
    uint32_t name_width, name_height, name_scale_milli;  // "Telescope" centred across it, its top at the top
};
#pragma pack(pop)

// The wait screen as NV12 (width x height, rows packed), or false if data isn't a whole one.
bool ComposeWaitScreen(const uint8_t* data, size_t size, uint32_t width, uint32_t height, std::vector<uint8_t>& nv12);

}  // namespace telescope
