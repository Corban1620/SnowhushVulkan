#version 450
#include "globals.glsl"
// signVertex from the Mac app's shaders: a ghost's sign, turned about its stick's foot.

// How far everything is jolted, in points.
// Where the stick's foot is, the picture's size in points and how far it is turned counterclockwise.

layout(location = 0) out vec2 vUv;
layout(location = 1) out vec2 vWorld;

layout(push_constant, std140) uniform Push {
    vec2 anchor;
    vec2 size;
    float angle;
    float ground;
} pc;

void main() {
    vec2 corner = vec2(float(gl_VertexIndex & 1), float(gl_VertexIndex >> 1));
    vec2 local = vec2((corner.x - 0.5) * pc.size.x, corner.y * pc.size.y);
    vec2 world = pc.anchor + vec2(local.x * cos(pc.angle) - local.y * sin(pc.angle), local.x * sin(pc.angle) + local.y * cos(pc.angle));
    gl_Position = vec4((world + shake) / viewSize * 2.0 - 1.0, 0.0, 1.0);
    // The picture's rows run down from its top.
    vUv = vec2(corner.x, 1.0 - corner.y);
    vWorld = world;
}
