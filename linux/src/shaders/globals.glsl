// The frame's shared values, filled once for every pass. A field is a vector of four floats where a plain number
// would do, so the block's layout is plain and its bytes are easy to follow.

layout(set = 0, binding = 0, std140) uniform Globals {
    // The simulation's size in points, the scale every size is drawn at, and the snow's clock.
    vec4 viewInfo;
    vec4 lightInfo;
    vec4 shadowInfo;
    // The colour the snow's surface glows, and how much.
    vec4 rimInfo;
    // The cursor's candle: where it is, how far it reaches and how bright it is.
    vec4 lanternInfo;
    // The pumpkins' candles: where each burns, how far its light reaches and how bright it is.
    vec4 pumpkinInfo[3];
    // How far everything is jolted, in points, and how wide a drift's columns are.
    vec4 shakeInfo;
    // The panel's rectangle, left, bottom, right, top.
    vec4 panelInfo;
    // One while leaves fall, one while they are cherry blossom petals, one while fallen leaves wither, one while
    // running drops and beads are drawn as glass from their field.
    vec4 modeInfo;
    // How far into Halloween night it is, then unused.
    vec4 extraInfo;
} g;

#define viewSize g.viewInfo.xy
#define scale g.viewInfo.z
#define time g.viewInfo.w
#define shadowTint g.shadowInfo
#define lantern g.lanternInfo
#define pumpkins g.pumpkinInfo
#define shake g.shakeInfo.xy
#define columnWidth g.shakeInfo.z
#define panel g.panelInfo
#define autumn g.modeInfo.x
#define blossom g.modeInfo.y
#define wilt g.modeInfo.z
