#version 450
#include "globals.glsl"
// pileVertex from windows/src/shaders.hlsl.

// How far everything is jolted, in points.

layout(location = 0) out vec2 vLocal;

layout(push_constant, std140) uniform Push {
    vec2 origin;
    float pileTop;
    float pileBottom;
    int columnCount;
    int heightOffset;
    int occluderCount;
    int occluderBase;
    float drift;
} pc;

void main() {
    vec2 corner = vec2(float(gl_VertexIndex & 1), float(gl_VertexIndex >> 1));
    vec2 p = vec2(corner.x * float(pc.columnCount) * columnWidth, mix(pc.pileBottom, pc.pileTop, corner.y));
    gl_Position = vec4((p + pc.origin + shake) / viewSize * 2.0 - 1.0, 0.0, 1.0);
    vLocal = p;
}
