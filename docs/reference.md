# ferrotype reference

Everything ferrotype does, in detail. For a quick overview, see the [README](../README.md).

## Command line

Headless batch export, using each photo's saved edits:

```
ferrotype export --out ./out *.NEF
ferrotype export --size 1080 --quality 85 *.NEF                    # Instagram-sized JPEGs
ferrotype export --out ./out --neg portra400 --tif scans/*.tif   # convert a roll of negatives
ferrotype contact --cols 6 --out roll1.jpg scans/*.tif           # contact sheet of a roll
ferrotype export --hdr --size 2048 *.NEF                         # HDR gain-map JPEGs
ferrotype stocks                                                  # list film stock ids
ferrotype ai-setup                                                # download the optional AI denoise
ferrotype pano --projection cylindrical IMG_1.jpg IMG_2.jpg IMG_3.jpg   # stitch a panorama
ferrotype lens *.NEF                                              # show the lens profile each photo gets
ferrotype export --lens --set instagram --watermark "© Me" --meta --roll 12 --name "{roll}-{n}" *.NEF
ferrotype export --print 8x10 --dpi 300 scans/*.tif               # sized for printing (also 20x30cm)
ferrotype export --out ./out ~/Photos/2026                        # a folder: subfolders kept as a tree
ferrotype rename --name "{date}-{camera}-{n}" --dry-run *.NEF     # preview a batch rename
ferrotype rename --undo .                                         # put the old names back
ferrotype hdr IMG_1.NEF IMG_2.NEF IMG_3.NEF                       # merge a bracket into IMG_1-hdr.exr
ferrotype hdr --auto ~/Photos/trip                                # find every bracket and merge each
```

Export flags: `--lens` applies the detected lens profile, `--set instagram|story|portfolio` uses a ready-made size, `--name` takes a pattern with {name} {n} {date} {time} {camera} {lens} {iso} {focal} {aperture} {shutter} {roll} {stock} (date and time are when the photo was taken), `--meta` writes camera, lens, roll and stock into JPEGs, `--watermark TEXT` and `--logo FILE` add a mark, and `--print SIZE --dpi N` fits the photo to a paper size.

## GPU and tests

**GPU:** the tone and colour stage runs on the GPU through wgpu: Vulkan (AMD, NVIDIA, Intel, integrated graphics and APUs on Linux and Windows), Metal (every Mac, Apple Silicon included) and DirectX 12 (Windows). Without a usable GPU, or if the driver reports an error, it quietly renders on the CPU instead; Shortcuts shows which one is in use. `FERROTYPE_GPU=off` turns the GPU off, `FERROTYPE_GPU=force` also accepts software renderers (for testing).

**Tests:** `cargo test`. `gpu_matches_cpu` checks that the GPU shader and the CPU code still give the same picture, so it fails when there is no GPU at all. On a machine without one, install a software GPU (Linux: `mesa-vulkan-drivers`) or set `FERROTYPE_SKIP_GPU_TEST=1`.

## Look

The look follows the Ferrotype brand kit (direction F): flat 1 px panels in ink (#141416 base, #1C1C1F panels, #2A2A2E lines) with bone (#ECE8E1) text. There is no accent colour: the selected tab, stock, frame or button is shown by inverting it to ink on bone, so the only coloured pixels are in the photo and the histogram. Type is IBM Plex Sans for labels, IBM Plex Mono for section labels and values, and Bricolage Grotesque 700 for the wordmark and empty-state headlines, all bundled (subset) under the SIL Open Font License. Hold the mouse on the photo to see the original.

### Icon and logo

`assets/brand/` holds the app icon (`icon.svg`, a light tile; `icon-dark.svg` for light desktops; `icon-small.svg`, a heavier-stroke tile for 16 and 24 px), `ferrotype.ico` (Windows, 16 to 256 px), `ferrotype.icns` (macOS, 32 to 1024 px), PNGs from 16 to 1024 px in `png/`, the bare mark (`mark.svg`, `mark-16.svg`) and the logo lockups (`logo-dark.svg`, `logo-light.svg`, `logo-stacked.svg`) with the wordmark converted to outlines. The window uses the 256 px PNG as its icon, and on Windows `build.rs` embeds `ferrotype.ico` in the .exe.

## What it does

- **Opens:** most camera RAW formats through `rawler` (CR2, CR3, NEF, ARW, RAF, ORF, RW2, PEF, DNG and more), plus JPEG, PNG, TIFF, WebP, BMP, EXR, HDR, JPEG XL, HEIC/HEIF and AVIF.
- **Navigate:** the mouse wheel zooms around the pointer, from fit up to 1600%. On a Mac trackpad, pinch zooms, two fingers pan, and scrolling is smooth; a zoomed view sharpens once you stop moving.
- **Library:** folder grid with thumbnails (it uses the JPEG embedded in RAW files), adjustable thumbnail size and drag-and-drop. Only the rows on screen are drawn and rendered thumbnails are cached, so big folders open and scroll quickly. Photos you've edited are marked with a bone dot. Rate with 0–5, pick with P, reject with X, clear with U, colour-label with 6–9 (Shift+6 purple), and filter by rating or picks. Right-click a photo for a virtual copy (a second set of edits for the same file). Contact sheet… makes one numbered image of the photos shown. Ctrl/Cmd-click or Shift-click selects several photos; right-click them to export them or **merge them into a panorama** (Auto, Cylindrical, Spherical or Perspective). The panorama is matched, levelled, exposure-balanced, blended and cropped, then saved next to the photos as a 16-bit TIFF. **Watch folder** opens new photos as they arrive (from a scanner or a tethered camera) and can apply the current look and film conversion to them.
- **Cull:** full-screen review at embedded-preview speed (C). Arrow keys step through, the next photos are loaded ahead. Shows the raw histogram and the share of clipped photosites per channel straight from the sensor data, a focus check (F) that marks the sharpest edges in green, and 2-up compare (Shift+C). Right-click to **rename** the selected photos from a pattern (the same tokens as export, with a live preview; RAW+JPEG pairs, sidecars and .xmp files move along, and Undo last rename puts them back), or to **merge an exposure bracket to HDR** (aligned, ghost-reduced, saved as a 32-bit EXR next to the photos; Find and merge HDR brackets groups a whole folder by capture time and exposure). Move rejects to `_rejects` moves rejected photos and their sidecars into a subfolder; nothing is deleted.
- **Ratings, rejects and labels are written to a standard `photo.xmp`** (xmp:Rating, -1 for rejected, and xmp:Label), so Lightroom, darktable, Capture One and Bridge see them. An existing Lightroom .xmp keeps all its other settings, and ratings changed in other apps are read back.
- **Develop:** the Lightroom slider set. Color sliders have gradient tracks computed from the same math as the edit itself, sampled 64 times along each track. That covers white balance (Planckian locus), the HSL mixer, color grading, calibration and negative balance, so the track shows what each value does. Color grading also has three hue/saturation wheels, and the tone curve has a live plot.
  - Basic: Temp, Tint (with an eyedropper: click something neutral, or press W), Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Texture, Clarity, Dehaze, Vibrance, Saturation
  - Tone curve: point curve for RGB and for red, green and blue separately (click to add a point, drag it, double-click to remove it), drawn over the histogram, with a large curve window for fine work and a Brightness only mode (luminosity: contrast without the colour shift), plus the parametric Highlights, Lights, Darks and Shadows
  - Color mixer: Hue / Saturation / Luminance for 8 colors
  - Color grading: Shadows, Midtones, Highlights, Global (hue, sat, lum), Blending, Balance
  - Detail: Sharpening (Amount, Radius, Detail, Masking), Noise reduction (Luminance, Detail, Color), AI denoise (Strength)
  - AI denoise is optional and not in the app itself: Download AI denoise (or `ferrotype ai-setup`) fetches ONNX Runtime 1.22 from its official PyPI wheel (checksum-verified), the RawNIND UtNet2 model (GPL-3.0, checksum-verified) used on RAW files, and the SCUNet real-noise model (Apache-2.0) used on JPEGs and other processed photos, about 140 MB in all, into the app's data folder. The first run on a photo takes a while on a CPU; the result is cached, so the Strength slider is instant afterwards and exports reuse it.
  - Lens corrections: profile corrections from the bundled lensfun database (about 1,500 lenses, CC BY-SA 3.0, see `data/LENSFUN-DATABASE-LICENSE.txt`), found from the photo's camera and lens and correcting distortion, colour fringing and vignetting, with amount sliders and a lens search for manual lenses and film scans. Plus manual Distortion, Vignetting and Defringe.
  - Transform: Vertical, Horizontal, Rotate (auto-zooms to hide corners), Scale, Flip, and 90° turns ([ and ]) that carry the crop, spots and masks along
  - Crop: choose Crop in the toolbar and drag the corners or move the frame on the photo. Aspect presets: Free, 1:1, 4:5, 2:3, 3:2, 16:9.
  - Heal and clone: choose Heal and click a spot or paint a stroke over a hair or scratch. The app picks a matching source nearby; drag the source outline to choose another, and turn it with Source angle. Heal blends the source's texture into the surrounding colour, Clone copies it as is. Right-click a mark to remove it. It works on film scans before inversion. Find dust marks small dark specks (dust on the scan or the sensor) as spots in one go; right-click any that aren't dust.
  - Masks: choose Mask, then Linear, Radial or Brush, or a Luminance range, Colour range (pick a colour on the photo) or the whole photo. Each mask has every local setting: white balance, the Basic tone and presence sliders, the colour mixer, a tone curve, sharpening, noise reduction and defringe. Shapes combine with Add, Subtract and Intersect, each can be inverted, and the mask has its own Opacity, an extra Feather and Invert. Drag the handles to adjust, show the red overlay to see what's covered, and hide a mask with its dot to compare. Older gradients and brush masks open as masks.
  - Colour samplers: choose Sample and click up to 8 points; their RGB values show under the histogram and follow your edits (right-click one to remove it).
  - Color look (LUT): load any 3D `.cube` file and set how strongly it applies.
  - Border: black or white frame. Width is set as a % of the long side, and the canvas can be padded to a fixed shape (1:1, 4:5, 9:16, 3:2) for Instagram or a portfolio. An optional caption is set in a band under the photo. The border is included in exports and synced with the other settings.
  - Effects: Post-crop vignette (Amount, Midpoint, Roundness, Feather, Highlights), Halation (Amount, Size, Threshold: the red glow around bright lights on film), Grain (Amount, Size, Roughness, and Grain in highlights: like real film, grain fades in bright areas). "Match a film stock" sets grain and halation to suit any of the 61 stocks.
  - Calibration: Shadows tint, Red/Green/Blue primary hue and saturation
  - Workflow: snapshots (named versions of a photo's settings to come back to), presets (7 built in, plus your own, plus imported Lightroom `.xmp` presets, with a Preset amount slider from 0 to 200% that also scales the preset's masks), history with undo and redo, 1:1 view (Z or double-click; drag to move around; it renders in the background so the app stays responsive), copy/paste settings, sync to folder, auto tone, before/after, a draggable split view and side-by-side views, a paper preview (Glossy, Lustre, Matte, Fine-art rag, Newsprint) that shows roughly how a print will look, clipping overlay, histogram, filmstrip
- **Negative:** 61 film stock profiles (Kodak, Fuji, CineStill, Ilford, Harman, Lomography, Foma, Agfa, Rollei, Reflx, generic C-41/ECN-2/B&W, and E-6 slide). It auto-detects the film base or lets you pick it from the rebate with an eyedropper. Looks: Neutral, Frontier, Noritsu, Linear. Balance sliders: density, contrast, cyan–red, magenta–green, yellow–blue, toe, shoulder. Lock the conversion to one frame and apply it to the whole roll so colors match. Scanning helpers: Find frame crops away the holder and film rebate, Split strip turns one scan of several frames into one photo per frame (virtual copies), Base from shot… takes the film base from a photo of blank film, and Find dust. **My stocks:** save a conversion as your own stock profile (a small `.ferrostock` file), import one someone shared, or right-click to share or delete it. Shared profiles leave out the film base of your scan, so they work on anyone's roll.
- **Export:** JPEG (quality 50 to 100, optionally HDR: an Ultra HDR / Adobe gain-map JPEG whose highlights go up to 2 stops brighter on HDR screens and that looks normal everywhere else), 16-bit TIFF or PNG, at full size or a long edge of 4096, 2048 or 1080 px, or a print size (paper sizes from 4×6 in to 50×70 cm at 240, 300 or 360 dpi, fit or fill). Optional text or logo watermark, file names from a pattern, and camera, lens, roll and film stock written into JPEGs. Exporting a folder can include its subfolders and keep the folder tree. Export sets make Instagram (1080×1350), Story (1080×1920) and Portfolio (2048 px) versions in one go, each in its own folder. Without those details, exports contain pixels only, and GPS location and serial numbers are never written. Exports never replace an existing file: if `IMG_1.jpg` exists (for example the camera JPEG of a RAW+JPEG pair), the export is saved as `IMG_1-2.jpg`.
- **Edits are non-destructive.** They are saved next to each photo as `photo.ext.ferrotype.json`. If that file is ever damaged, it is kept as `.ferrotype.json.damaged` rather than overwritten. Healed spots, heal strokes and masks follow the photo when you rotate, straighten or change the lens correction. A photo without one but with a Lightroom `photo.xmp` opens with those settings (approximately: the sliders map one to one, but the maths differs). Paste, presets and sync copy the look but keep each photo's crop and its negative conversion. Use Apply to roll to copy a conversion.

Shortcuts: all single-key shortcuts can be changed in **Shortcuts** in the top bar (saved to `~/.config/ferrotype/shortcuts.json`). Defaults: 0–5 stars, P/X/U flags, 6–9 labels, C cull, G library, D develop, Z 1:1, `\` before/after, J clipping, R crop, Q heal, M linear mask, K brush mask, W white balance picker, [ ] rotate, Shift+V virtual copy, Shift+S snapshot, F focus check, ←/→ next and previous while browsing. Fixed: Ctrl/Cmd+Z undo, Ctrl/Cmd+Shift+Z redo, Ctrl/Cmd+C or V copy and paste settings, Ctrl/Cmd+←/→ previous and next photo. On a slider: double-click resets it, and ←/→ nudge it.

## Not built yet (from the research spec)

- AI subject and sky masks, red-eye, Upright auto-perspective
- More of the pipeline on the GPU (geometry, blurs, sharpening and noise reduction still run on the CPU with rayon; LUTs too)
- Infrared-channel dust removal (Find dust looks at the picture instead)
- ICC printer profiles (the paper preview is an approximation), keywords in XMP, metadata in TIFF/PNG and HDR exports (JPEG only)
- The built-in stock profiles are hand-tuned approximations, not measured curves.
