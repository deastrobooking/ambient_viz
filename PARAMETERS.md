# Visualizer Parameter Guide

Simple one-sentence descriptions of every parameter exposed on the front-end
interface (the dev panel / automation lanes in `static/index.html`). Default
values are shown in parentheses.

## Slice tears (the horizontal "bars")

- **sliceTrigger** *(0.065)* — How easily a bass hit spawns a burst of slice-tear bars; lower = bars trigger more often.
- **midSliceTrigger** *(0.09)* — Same as above but driven by mid-range frequencies instead of bass.
- **sliceBurstScale** *(1.0)* — How many bars each burst spawns (scaled gently at 0.25×, so 1.0 ≈ a few bars).
- **monoBarChance** *(0.125)* — Probability that any given bar renders as the desaturated mono variant rather than colored.

## Sparks (the radial particle bursts)

All four are multipliers on the original hardcoded behavior, so `1.0` = the legacy look.

- **sparkBurstScale** *(1.0)* — Number of particles per burst (0 = no sparks, 2 = double the count).
- **sparkTrigger** *(1.0)* — Audio-onset sensitivity for firing bursts; higher = needs a bigger spike, so fewer bursts; lower = more frequent.
- **sparkDecay** *(1.0)* — How fast particles fade; higher = shorter-lived, lower = they linger.
- **sparkCooldown** *(1.0)* — Minimum gap between bursts; higher = sparser, lower = more frequent.

## Beat-locked cycling

- **bpm** *(80)* — Tempo that drives all beat-locked visuals (currently the twist cycle).
- **kicksPerColorCycle** *(64)* — Number of beats before the palette advances to the next color.
- **kicksPerShapeCycle** *(16)* — Number of beats before the morphing silhouette advances to the next shape.
- **kicksPerTwist** *(8)* — Number of beats in one full twist cycle (wind up, then unwind).

## The morphing shape & lattice

- **silhouette** *(car)* — Which outline (car, head, etc.) the morphing shape is currently drawn as.
- **latticeSpacing** *(24)* — Spacing between the grid points that the shape is stamped across.
- **jitterPx** *(26)* — Treble-reactive horizontal wobble applied to the lattice points, in pixels.
- **rowCorruptAmount** *(2.6)* — Bass-reactive horizontal misalignment of lattice rows, for a torn/corrupted look.

## 3D mesh & twist

- **mesh3dCount** *(2)* — How many 3D wireframe mesh objects fly through the scene.
- **mesh3dRotSpeed** *(0.0075)* — How fast those 3D meshes spin on their axis.
- **maxTwistDeg** *(720)* — Maximum rotation, in degrees, of the beat-driven scene twist.
- **twistPinch** *(0.3)* — How strongly perspective pinches at the peak of each twist.
- **twistSpringHz** *(1.0)* — Springiness of the twist's easing toward its beat target (higher = snappier).

## Glitch & freeze

- **onsetThreshold** *(0.07)* — Audio-onset sensitivity for randomly triggering frame-freeze / block-shuffle glitches.
- **freezeFramesMax** *(12)* — Longest a frame-freeze glitch is allowed to hold, in frames.
- **freezeMonoChance** *(0.125)* — Probability a frozen frame is rendered in monochrome.
- **flashTrigger** *(0.18)* — Bass-rise threshold that fires random invert/strobe flashes; lower = flashes more.

## Texture & overlays

- **grainAlpha** *(0.46)* — Opacity of the film-grain texture over the image.
- **ditherAmount** *(1.0)* — Strength of the dithering pattern blended across the whole frame.
- **scanlineAlpha** *(0.42)* — Opacity of the horizontal CRT scanline overlay.
- **saturation** *(1.0)* — Overall color saturation (0 = grayscale, 1 = full color).
- **colorLerp** *(0.01)* — How fast the palette blends from one color to the next each frame; lower = slower fades.

## Touch / sensor reactivity

- **touchRiseS** *(8.0)* — Seconds for a touch-electrode color to fade *in* when triggered.
- **touchFallS** *(18.0)* — Seconds for a touch-electrode color to fade *out* after release.

## Render resolution

- **bitmapHeight** *(1080)* — Render-buffer height cap in pixels; lower values coarsen the dither/scanline look and cost less GPU.
- **distanceToBitmap** *(off)* — When on, lets the distance sensor lower bitmapHeight as someone approaches (kiosk only).
