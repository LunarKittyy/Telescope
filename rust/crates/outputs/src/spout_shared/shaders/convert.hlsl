// BGRA, RGBA or BGRX to I420 planes (R8 targets) on the card, with the BT.601 limited-range maths of convert.rs bgra_to_i420. Rebuild the .cso files with compile.ps1.

Texture2D<float4> source : register(t0);

float4 vs_main(uint id : SV_VertexID) : SV_Position
{
    // One triangle that covers the whole target, corners (-1, 1), (3, 1) and (-1, -3)
    float2 corner = float2((id << 1) & 2, id & 2);
    return float4(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
}

float ps_y(float4 position : SV_Position) : SV_Target
{
    float3 rgb = source.Load(int3(int2(position.xy), 0)).rgb * 255.0;
    return (16.0 + dot(rgb, float3(0.257, 0.504, 0.098))) / 255.0;
}

struct Chroma
{
    float u : SV_Target0;
    float v : SV_Target1;
};

Chroma ps_uv(float4 position : SV_Position)
{
    uint width, height;
    source.GetDimensions(width, height);
    int2 last = int2(width - 1, height - 1);
    int2 at = int2(position.xy) * 2;
    // The 2x2 block of this sample; the edge of an odd size repeats its own pixels, which averages the same as the pixels there are
    float3 sum = source.Load(int3(min(at, last), 0)).rgb
               + source.Load(int3(min(at + int2(1, 0), last), 0)).rgb
               + source.Load(int3(min(at + int2(0, 1), last), 0)).rgb
               + source.Load(int3(min(at + int2(1, 1), last), 0)).rgb;
    float3 rgb = sum * (255.0 / 4.0);
    Chroma o;
    o.u = (128.0 + dot(rgb, float3(-0.148, -0.291, 0.439))) / 255.0;
    o.v = (128.0 + dot(rgb, float3(0.439, -0.368, -0.071))) / 255.0;
    return o;
}
