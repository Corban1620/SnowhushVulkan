### Snowhush's Linux renderer, rewritten from OpenGL ES to Vulkan.
#### Status: Complete - Vulkan only, with no OpenGL in the drawing path.

> **Compatibility**
>
> Confirmed working only on **KDE Plasma 6.8 on Wayland, on Arch Linux** (tested on CachyOS, an Arch-based distribution) with an Intel integrated GPU.
>
> - **Wayland with `wlr-layer-shell` is required. X11 is not supported.**
> - Headless **Sway** was also verified using software Vulkan (llvmpipe). **Hyprland** and **niri** are expected to work, since the platform integration is unchanged, but were not re-tested after the rewrite.
> - Other Linux distributions with a Vulkan 1.1+ driver and the usual Wayland libraries should work in principle, but only Arch-based Linux has been confirmed.
> - AMD and NVIDIA GPUs were not tested; an Intel integrated GPU and llvmpipe were.
> - Note that testing for AMD GPUs will be confirmed soon.
> - [Jump to Vulkan vs OpenGL ES Benchmark](#vulkan-vs-opengl-es)
> - [Jump to Rewrite and Optimizations](#rewrite-and-optimizations)


<p align="center">
  <img src="site/assets/icon-512.png" width="128" height="128" alt="Snowhush icon">
</p>

<h1 align="center">Snowhush</h1>

<p align="center">It's snowing on your desktop.</p>

<p align="center">
  <a href="https://github.com/mbezruchko/Snowhush/releases/latest/download/Snowhush.dmg"><b>Download for macOS</b></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/mbezruchko/Snowhush/releases/latest/download/Snowhush-Setup.exe"><b>Download for Windows</b></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/mbezruchko/Snowhush/releases/latest/download/Snowhush-x86_64.AppImage"><b>Download for Linux</b></a>
  &nbsp;·&nbsp;
  <a href="https://snowhush.pages.dev">snowhush.pages.dev</a>
  <br>
  <sub>macOS 14 or later · Windows 10 or 11 · Linux with KDE Plasma 6, Hyprland, Sway or niri · free, source available</sub>
</p>

https://github.com/user-attachments/assets/291c3cd5-3563-4b86-8b66-dfd924f980fc

A tiny app that lives in the menu bar on macOS and in the tray on Windows and Linux. Soft snow drifts across your screen, settles along the tops of your windows and slips off when you drag them. Sweep the drift aside with your cursor.

## What the snow does

- **Falls.** Every flake keeps its own pace and sways in a slow breeze.
- **Settles.** It piles up and slumps to a natural slope along the bottom of the screen, on any window with room above it, and on the Dock.
- **Moves.** Push the cursor through a drift and it parts and sprays. Drag or shake a window and its snow slides off and lands below.
- **Drops.** Close or minimize a window and its snow drops at once, in lumps that burst into powder where they land.
- **Melts.** With Melt Snow on, the drift thaws slowly, so it never takes over your screen.
- **Hides.** Screenshots and screen sharing show your desktop without it, unless you turn on Show in Screenshots.
- **Rests.** About 6% of one processor core and 65 MB of memory, and less on battery or in Low Power Mode. It pauses while your screen sleeps, locks or is covered.

## Autumn Leaves

Choose Mode › Autumn Leaves and leaves fall instead of snow. They turn and swing on the way down, settle in heaps you can see are made of leaves, scatter under the cursor and come off a window you shake or drag. Melt Snow, Snow on Windows and Clear Snow become Wither Leaves, Leaves on Windows and Clear Leaves.

## Rain

Choose Mode › Rain and rain beats down instead of snow, splashing where it lands. On the tops of your windows, the Dock and the taskbar it spatters the glass with spray and sets drops running down it like beads of water on a window: they catch on specks, gather the spray they meet and lurch on, leaving a wet film and a trail of droplets. Others hang under bottom edges until they grow heavy enough to let go. Drag the cursor through them to smear them, or shake or drag a window to throw them off. Rain doesn't pile up, so Accumulate, Melt Snow and Clear Snow step aside, and Snow on Windows becomes Rain on Windows.

## Cherry Blossom

Choose Mode › Cherry Blossom and pink petals fall instead of snow. Curved like a spoon, they glint as they tumble and glow where the light comes through them, and now and then a whole flower on its stalk spins down. They heap up and scatter just as leaves do, and with Wilt Petals on, the fallen ones brown from their edges before they go. Melt Snow, Snow on Windows and Clear Snow become Wilt Petals, Petals on Windows and Clear Petals.

## Halloween

From October 30 until dawn on November 1 the snow turns spooky: jack-o'-lanterns glow in the drift and ghosts pop up out of it with signs. Choose Mode › Halloween to have it any other day, as long as snow is falling.

Everything else lives in the menu bar: Intensity and Opacity sliders (how much falls and how see-through it is; on Linux and in the plain Windows menu, submenus of steps), Mode (Snow, Autumn Leaves, Rain, Cherry Blossom, and Halloween on top of snow) and Clear Snow, with the rest under Settings: Accumulate, Melt Snow, Snow on Windows, Deep Interaction (on Windows and the Mac, the snow also settles on a selection dragged across the desktop), Behind Windows, Show over Full Screen, Show in Screenshots, Max Performance (draws every frame at the display's full rate) and Open at Login. On Windows and the Mac, Settings stays open while you switch its options. On Windows, Behind Taskbar lets the snow settle at the bottom of the screen under a see-through taskbar, such as one made clear with TranslucentTB.

## Building

```sh
./build.sh            # build/Snowhush.app, signed ad hoc
open build/Snowhush.app
windows/build.sh      # dist/Snowhush-Setup.exe, cross-compiled from macOS
```

### Linux

`linux/` is a native version for Wayland: KDE Plasma 6, Hyprland, Sway and niri. It shares the snow's physics with the Windows version and draws with Vulkan in a layer-shell overlay. Wayland doesn't tell apps where other windows are or where the pointer is, so each compositor is asked in its own way: on KDE, Snowhush loads a small KWin script (`linux/src/snowhush.js`) while it runs, which reports both; Hyprland answers over its IPC socket, which tells both too; Sway and niri answer over theirs, though they don't tell where the pointer is, and niri only where floating windows are, so on niri only those collect snow.

Each release carries an AppImage that runs on any distribution, a `.deb` and an `.rpm`. On Arch, `makepkg -si` in `linux/packaging/aur` installs the release's build as a package. To build it yourself:

```sh
cd linux && cargo build --release   # linux/target/release/snowhush
# Shaders are built with glslc when it is on PATH; checked-in SPIR-V is used otherwise.
cd linux/packaging && makepkg -si   # Arch: builds and installs it with its icon and menu entry
```

`linux/bench/run.sh sway|niri|kwin` tries it in a compositor inside a container, without a display.

Everything lives in the tray, as on Windows. Settings go to `~/.config/snowhush/settings`, Open at Login to `~/.config/autostart/snowhush.desktop`, the log to `~/.local/state/snowhush/snowhush.log`.

## Vulkan vs OpenGL ES

Measured on **KDE Plasma 6.8.2, Wayland, Arch-based Linux (CachyOS)**, with an **Intel RPL-U integrated GPU** at 1920×1080. Each build ran in a fresh process with an 8-second warm-up and a 30-second sample, twice in alternating order. RSS and PSS came from `/proc`, sampled once per second.

Settings used for both builds:

```ini
intensity=100
accumulate=1
meltSnow=0
snowOnWindows=0
behindWindows=0
showOverFullScreen=1
showInScreenshots=1
halloweenOn=0
halloweenSkipped=0
mode=0
autumn=0
performance=1
fallingOpacity=100
```

| Metric | Original OpenGL ES | Vulkan | Change |
|---|---:|---:|---:|
| RSS average | 100.66 MiB | **25.71 MiB** | **−74.5%** |
| RSS peak | 100.82 MiB | **25.75 MiB** | **−74.5%** |
| PSS average | 37.76 MiB | **14.12 MiB** | **−62.6%** |
| PSS peak | 37.89 MiB | **14.18 MiB** | **−62.6%** |

Note: This data is the Vulkan pipeline **with** the optimisations below

For completeness, CPU over the same run was **4.09% → 3.90% of one core**, and per-process iGPU render-engine time was **2.18% → 2.26%**.

## Rewrite and optimizations

Changes:

- Replaced EGL, OpenGL ES, `glow` and `khronos-egl` with pure Vulkan through `ash`.
- Added Vulkan instance/device selection, `VK_KHR_wayland_surface`, swapchains, render passes, framebuffers, pipelines, descriptor sets, push constants, transfer paths and per-display surfaces.
- Ported every shader to Vulkan GLSL 450 with explicit locations and descriptor layouts.
- Added `linux/build.rs`: shaders compile with `glslc` when available, otherwise checked-in SPIR-V is used.
- Added new fade shaders and a shared globals block; updated packaging, the headless bench container and this README.

Optimizations made after the first Vulkan benchmark:

- Removed the duplicate fence wait, leaving one wait per frame.
- Removed the per-frame temporary copy of the sprite buffer; sprites upload directly.
- Replaced the height texture, staging buffer and per-frame image barriers with a host-visible storage buffer read by the pile shader.
- Kept resource destruction behind the frame fence, so the faster path remains safe.

Validation performed after the rewrite: Vulkan validation layer reported no errors in snow, Halloween and rain modes; headless Sway with llvmpipe verified snow, window drifts, rain/glass, resize and covered/fade transitions.

## License

[PolyForm Noncommercial 1.0.0](LICENSE): use, share and change Snowhush freely, keeping its copyright notice with it. Selling it, or anything built on its code, isn't allowed. Versions up to 0.6.1 were released under the MIT License.
