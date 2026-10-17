cbuffer Globals : register(b0) {
    float2 pixel_to_clip;
    float2 contrast; // enhanced-contrast strength for dark and for light text
};
Texture2D<float> coverage_atlas : register(t0);
Texture2D<float4> color_atlas : register(t1);

struct Quad {
    float4 rect : RECT;          // x, y, width, height in pixels
    float2 uv : UV;              // top-left texel in the atlas (glyphs)
    float4 color : COLOR;        // straight alpha
    uint kind : KIND;
    float param : PARAM;         // pattern period (underlines)
};
struct Pixel {
    float4 pos : SV_Position;
    float2 uv : UV;
    nointerpolation float4 color : COLOR;
    nointerpolation float4 rect : RECT;
    nointerpolation uint kind : KIND;
    nointerpolation float param : PARAM;
};

Pixel vs_main(Quad q, uint id : SV_VertexID) {
    float2 corner = float2(id & 1, id >> 1);
    float2 p = q.rect.xy + corner * q.rect.zw;
    Pixel o;
    o.pos = float4(p.x * pixel_to_clip.x - 1, 1 - p.y * pixel_to_clip.y, 0, 1);
    o.uv = q.uv + corner * q.rect.zw;
    o.color = q.color;
    o.rect = q.rect;
    o.kind = q.kind;
    o.param = q.param;
    return o;
}

// Coverage is blended in gamma space, which makes light text on a dark
// background look thinner than the same text dark on light. DirectWrite
// counters that with an "enhanced contrast" curve; we scale its strength with
// the text's brightness.
float enhance_contrast(float coverage, float3 color) {
    float k = lerp(contrast.x, contrast.y, dot(color, float3(0.2126, 0.7152, 0.0722)));
    return coverage * (k + 1) / (coverage * k + 1);
}

float4 ps_main(Pixel p) : SV_Target {
    float a = p.color.a;
    if (p.kind == 1) {
        a *= enhance_contrast(coverage_atlas.Load(int3(p.uv, 0)), p.color.rgb);
    } else if (p.kind == 2) {
        return color_atlas.Load(int3(p.uv, 0)) * a;
    } else if (p.kind == 3) {
        // A sine wave through the middle of the band, phased by absolute x so
        // it continues smoothly from one cell to the next.
        float thickness = max(1, p.rect.w / 3);
        float amplitude = (p.rect.w - thickness) / 2;
        float center = p.rect.y + p.rect.w / 2 + amplitude * sin(p.pos.x * 6.2831853 / p.param);
        a *= saturate(thickness / 2 + 0.5 - abs(p.pos.y - center));
    } else if (p.kind == 4) {
        a *= fmod(floor(p.pos.x), 2 * p.param) < p.param ? 1 : 0;
    } else if (p.kind == 5) {
        a *= fmod(floor(p.pos.x), p.param) < p.param * 0.6 ? 1 : 0;
    }
    return float4(p.color.rgb * a, a); // premultiplied
}
