// Spectra preview shaders (compute, cs_5_0).
//
// Every kernel writes its result into an R8G8B8A8 texture with the channels
// stored in B,G,R,A order, which is the byte layout GPUI expects. That way the
// CPU only has to read the bytes back; no per-pixel work.
//
// The FSR kernels are compiled with AMD's ffx_a.h / ffx_fsr1.h prepended (see
// gpu.rs); the other kernels don't need them.

cbuffer Params : register(b0)
{
    uint4 Con0;
    uint4 Con1;
    uint4 Con2;
    uint4 Con3;
    uint2 SrcSize; // size of the region of Src to use (from the top-left)
    uint2 DstSize;
};

Texture2D<float4> Src : register(t0);
RWTexture2D<float4> Dst : register(u0);
SamplerState LinearClamp : register(s0);

float4 ToBgra(float3 c) { return float4(c.b, c.g, c.r, 1.0); }

// Exact sRGB transfer functions.
float3 SrgbToLinear(float3 c) { return c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4); }
float3 LinearToSrgb(float3 c) { return c <= 0.0031308 ? c * 12.92 : 1.055 * pow(c, 1.0 / 2.4) - 0.055; }

#ifdef KERNEL_COPY
// 1:1 copy (native resolution): channel swap only, bit-exact.
[numthreads(8, 8, 1)]
void Main(uint3 id : SV_DispatchThreadID)
{
    if (any(id.xy >= DstSize)) return;
    Dst[id.xy] = ToBgra(Src.Load(int3(id.xy, 0)).rgb);
}
#endif

#ifdef KERNEL_DOWNSCALE
// Area-weighted box filter in linear light: every source pixel contributes in
// proportion to how much of it the output pixel covers, and averaging happens
// on linear values so fine detail keeps its true brightness and colour.
[numthreads(8, 8, 1)]
void Main(uint3 id : SV_DispatchThreadID)
{
    if (any(id.xy >= DstSize)) return;
    float2 scale = float2(SrcSize) / float2(DstSize);
    float2 p0 = float2(id.xy) * scale;
    float2 p1 = p0 + scale;
    int2 a = int2(floor(p0));
    int2 b = min(int2(ceil(p1)), int2(SrcSize));
    // Bound the work for huge reductions by striding (weights stay exact per cell).
    int2 stride = max(int2(1, 1), (b - a + 15) / 16);

    float3 sum = 0;
    float total = 0;
    for (int y = a.y; y < b.y; y += stride.y)
    {
        float wy = min(float(y + stride.y), p1.y) - max(float(y), p0.y);
        for (int x = a.x; x < b.x; x += stride.x)
        {
            float wx = min(float(x + stride.x), p1.x) - max(float(x), p0.x);
            float w = max(wx, 0) * max(wy, 0);
            sum += SrgbToLinear(saturate(Src.Load(int3(x, y, 0)).rgb)) * w;
            total += w;
        }
    }
    Dst[id.xy] = ToBgra(LinearToSrgb(sum / max(total, 1e-8)));
}
#endif

#ifdef KERNEL_EASU
// FSR 1.0 EASU (edge-adaptive spatial upsampling). Operates on the stored
// (gamma-encoded) values, as AMD recommends, and writes to an RGBA16F
// intermediate in normal R,G,B order for RCAS.
AF4 FsrEasuRF(AF2 p) { return Src.GatherRed(LinearClamp, p); }
AF4 FsrEasuGF(AF2 p) { return Src.GatherGreen(LinearClamp, p); }
AF4 FsrEasuBF(AF2 p) { return Src.GatherBlue(LinearClamp, p); }
FSR_INCLUDE
[numthreads(8, 8, 1)]
void Main(uint3 id : SV_DispatchThreadID)
{
    if (any(id.xy >= DstSize)) return;
    AF3 c;
    FsrEasuF(c, id.xy, Con0, Con1, Con2, Con3);
    Dst[id.xy] = float4(c, 1.0);
}
#endif

#ifdef KERNEL_RCAS
// FSR 1.0 RCAS (robust contrast-adaptive sharpening) on EASU's output.
AF4 FsrRcasLoadF(ASU2 p) { return Src.Load(int3(p, 0)); }
void FsrRcasInputF(inout AF1 r, inout AF1 g, inout AF1 b) {}
FSR_INCLUDE
[numthreads(8, 8, 1)]
void Main(uint3 id : SV_DispatchThreadID)
{
    if (any(id.xy >= DstSize)) return;
    AF3 c;
    FsrRcasF(c.r, c.g, c.b, id.xy, Con0);
    Dst[id.xy] = ToBgra(c);
}
#endif
