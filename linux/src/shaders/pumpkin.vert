#version 450
#include "globals.glsl"
// pumpkinVertex from the Mac app's shaders: a quad reaching past the pumpkin on every side but the bottom, for its glow.

// How far everything is jolted, in points.
// Where the middle of its bottom rests and its size, in points.

layout(location = 0) out vec2 vUv;
layout(location = 1) out vec2 vWorld;

layout(push_constant, std140) uniform Push {
    vec2 anchor;
    vec2 size;
    vec2 wick;
    float seed;
} pc;

void main() {
    vec2 corner = vec2(float(gl_VertexIndex & 1), float(gl_VertexIndex >> 1));
    vec2 uv = vec2(mix(-0.6, 1.6, corner.x), mix(0.0, 1.8, corner.y));
    vec2 world = pc.anchor + (uv - vec2(0.5, 0.0)) * pc.size;
    gl_Position = vec4((world + shake) / viewSize * 2.0 - 1.0, 0.0, 1.0);
    // The pictures' rows run down from their top.
    vUv = vec2(uv.x, 1.0 - uv.y);
    vWorld = world;
}
