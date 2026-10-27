// Lays the built-in wait screen out for the size an app picked, the way vcam._default_screen() does.
#include "wait_screen.h"

#include <algorithm>
#include <cmath>
#include <string.h>

namespace telescope {

namespace {

// Which source pixels one destination pixel averages, and how much of each
struct Taps {
    int first = 0;
    std::vector<float> weights;
};

// A tent filter, widened when shrinking so every source pixel counts
std::vector<Taps> AxisTaps(double scale, double offset, int from, int to) {
    double radius = std::max(1.0, 1.0 / scale);
    std::vector<Taps> taps(static_cast<size_t>(to - from));
    for (int d = from; d < to; d++) {
        double centre = (d + 0.5 - offset) / scale - 0.5;
        Taps& t = taps[static_cast<size_t>(d - from)];
        t.first = static_cast<int>(std::ceil(centre - radius));
        int last = static_cast<int>(std::floor(centre + radius));
        float sum = 0;
        for (int i = t.first; i <= last; i++) {
            float w = static_cast<float>(std::max(0.0, 1.0 - std::abs(i - centre) / radius));
            t.weights.push_back(w);
            sum += w;
        }
        if (sum > 0) {
            for (float& w : t.weights) w /= sum;
        }
    }
    return taps;
}

// A picture resized by scale with its top left at (left, top), as much of it as lands on a dw x dh plane
struct Layer {
    int x = 0, y = 0, w = 0, h = 0, channels = 1;
    std::vector<float> px;
    float at(int col, int row, int c) const { return px[(static_cast<size_t>(row) * w + col) * channels + c]; }
};

Layer Resize(const uint8_t* src, int sw, int sh, int channels, double scale, double left, double top, int dw, int dh) {
    Layer layer;
    layer.channels = channels;
    layer.x = std::max(0, static_cast<int>(std::floor(left)));
    layer.y = std::max(0, static_cast<int>(std::floor(top)));
    int right = std::min(dw, static_cast<int>(std::ceil(left + sw * scale)));
    int bottom = std::min(dh, static_cast<int>(std::ceil(top + sh * scale)));
    if (right <= layer.x || bottom <= layer.y) return layer;
    layer.w = right - layer.x;
    layer.h = bottom - layer.y;
    std::vector<Taps> cols = AxisTaps(scale, left, layer.x, right);
    std::vector<Taps> rows = AxisTaps(scale, top, layer.y, bottom);
    auto clampTo = [](int i, int n) { return std::min(std::max(i, 0), n - 1); };
    int firstRow = clampTo(rows.front().first, sh);
    int lastRow = clampTo(rows.back().first + static_cast<int>(rows.back().weights.size()) - 1, sh);
    // Across first, for only the source rows the result needs
    std::vector<float> across(static_cast<size_t>(lastRow - firstRow + 1) * layer.w * channels, 0.0f);
    for (int r = firstRow; r <= lastRow; r++) {
        const uint8_t* line = src + static_cast<size_t>(r) * sw * channels;
        float* out = &across[static_cast<size_t>(r - firstRow) * layer.w * channels];
        for (int x = 0; x < layer.w; x++) {
            const Taps& t = cols[static_cast<size_t>(x)];
            for (size_t j = 0; j < t.weights.size(); j++) {
                const uint8_t* p = line + static_cast<size_t>(clampTo(t.first + static_cast<int>(j), sw)) * channels;
                for (int c = 0; c < channels; c++) out[x * channels + c] += t.weights[j] * p[c];
            }
        }
    }
    layer.px.assign(static_cast<size_t>(layer.w) * layer.h * channels, 0.0f);
    for (int y = 0; y < layer.h; y++) {
        const Taps& t = rows[static_cast<size_t>(y)];
        float* out = &layer.px[static_cast<size_t>(y) * layer.w * channels];
        for (size_t j = 0; j < t.weights.size(); j++) {
            const float* in = &across[static_cast<size_t>(clampTo(t.first + static_cast<int>(j), sh) - firstRow) *
                                      layer.w * channels];
            for (int i = 0; i < layer.w * channels; i++) out[i] += t.weights[j] * in[i];
        }
    }
    return layer;
}

uint8_t ToByte(float v) { return static_cast<uint8_t>(std::min(255.0f, std::max(0.0f, v + 0.5f))); }

// Onto a plane width pixels wide with the layer's channels interleaved
void Copy(const Layer& layer, uint8_t* plane, int width) {
    for (int y = 0; y < layer.h; y++) {
        uint8_t* row = plane + (static_cast<size_t>(layer.y + y) * width + layer.x) * layer.channels;
        for (int x = 0; x < layer.w; x++) {
            for (int c = 0; c < layer.channels; c++) row[x * layer.channels + c] = ToByte(layer.at(x, y, c));
        }
    }
}

// colour (channels of it, interleaved like the plane) blended in as far as the mask layer covers each pixel
void Tint(const Layer& mask, uint8_t* plane, int width, const uint8_t* colour, int channels) {
    for (int y = 0; y < mask.h; y++) {
        uint8_t* row = plane + (static_cast<size_t>(mask.y + y) * width + mask.x) * channels;
        for (int x = 0; x < mask.w; x++) {
            float a = mask.at(x, y, 0) / 255.0f;
            for (int c = 0; c < channels; c++) {
                uint8_t& p = row[x * channels + c];
                p = ToByte(p + (colour[c] - p) * a);
            }
        }
    }
}

}  // namespace

bool ComposeWaitScreen(const uint8_t* data, size_t size, uint32_t width, uint32_t height, std::vector<uint8_t>& nv12) {
    if (!data || size < sizeof(WaitScreenHeader) || width < 2 || height < 2) return false;
    WaitScreenHeader h;
    memcpy(&h, data, sizeof(h));
    if (h.magic != kWaitScreenMagic || h.version != kWaitScreenVersion || h.lens_size % 2 || !h.lens_scale_milli ||
        !h.name_scale_milli || !h.screen_width || !h.screen_lens || !h.screen_with_name || h.lens_size > 8192 ||
        h.name_width > 8192 || h.name_height > 8192) {
        return false;
    }
    size_t lens = static_cast<size_t>(h.lens_size) * h.lens_size;
    size_t mask = static_cast<size_t>(h.name_width) * h.name_height;
    if (size != sizeof(h) + lens + lens / 2 + mask) return false;
    const uint8_t* lensY = data + sizeof(h);
    const uint8_t* lensUV = lensY + lens;
    const uint8_t* name = lensUV + lens / 2;

    int w = static_cast<int>(width), ht = static_cast<int>(height);
    nv12.resize(static_cast<size_t>(w) * ht * 3 / 2);
    uint8_t* y = nv12.data();
    uint8_t* uv = y + static_cast<size_t>(w) * ht;
    memset(y, static_cast<int>(h.bg_y), static_cast<size_t>(w) * ht);
    for (size_t i = 0; i + 1 < static_cast<size_t>(w) * (ht / 2); i += 2) {
        uv[i] = static_cast<uint8_t>(h.bg_u);
        uv[i + 1] = static_cast<uint8_t>(h.bg_v);
    }

    // vcam.screen_layout()
    double widthShare = h.width_share_milli / 1000.0, heightShare = h.height_share_milli / 1000.0;
    auto fit = [&](uint32_t group) { return std::min(w * widthShare / h.screen_width, ht * heightShare / group); };
    double k = fit(h.screen_with_name);
    bool withName = h.name_px * k >= h.name_min_px;
    if (!withName) k = fit(h.screen_lens);
    double cx = w / 2.0, cy = (ht - (withName ? h.screen_with_name : h.screen_lens) * k) / 2 + h.centre * k;

    double s = k / (h.lens_scale_milli / 1000.0);
    double left = cx - h.lens_size * s / 2, top = cy - h.lens_size * s / 2;
    int half = static_cast<int>(h.lens_size / 2);
    Copy(Resize(lensY, static_cast<int>(h.lens_size), static_cast<int>(h.lens_size), 1, s, left, top, w, ht), y, w);
    Copy(Resize(lensUV, half, half, 2, s, left / 2, top / 2, w / 2, ht / 2), uv, w / 2);
    if (!withName) return true;

    double n = k / (h.name_scale_milli / 1000.0);
    left = cx - h.name_width * n / 2;
    top = cy + h.name_top * k;
    int nw = static_cast<int>(h.name_width), nh = static_cast<int>(h.name_height);
    const uint8_t luma[1] = {static_cast<uint8_t>(h.text_y)};
    const uint8_t chroma[2] = {static_cast<uint8_t>(h.text_u), static_cast<uint8_t>(h.text_v)};
    Tint(Resize(name, nw, nh, 1, n, left, top, w, ht), y, w, luma, 1);
    Tint(Resize(name, nw, nh, 1, n / 2, left / 2, top / 2, w / 2, ht / 2), uv, w / 2, chroma, 2);
    return true;
}

}  // namespace telescope
