# Kiosk Electronics Enclosure — 3D-printed (Fusion 360 → Bambu Studio)

Single FDM-printed box holding **Board A** (Daisy audio breakout) and **Board B**
(kiosk sensors) side by side, split by an internal divider. Hinged snap-top lid.
Target material: **PLA** (see notes). Designed in Fusion 360; decorative top text/line
added in **Bambu Studio**, not Fusion.

> Status: design spec / parameter scaffold. Values tagged **(MEASURE)** are placeholders
> to overwrite with caliper readings; **(CHOICE)** are recommended starting design values.

---

## 1. Topology

```
            ┌──────────── FRONT WALL (4 openings) ─────────────┐
            │  [USB] [audio 3.5] [MIDI 3.5]        [Cat6 PT]    │
   ┌────────┴──────────────────────┬───────────────────────────┴────────┐
   │        Board A (slide rails)   │  divider  │   Board B (screwed)     │
   │                                │           │   on ~15mm bosses       │
   └────────┬──────────────────────┴───────────────────────────┬────────┘
            │  [2× Dupont slot]              [5× Dupont slot]    │
            └──────────── REAR WALL (hinge + 2 openings) ────────┘
                          ▲ stub-pivot hinge axis ▲
```

- **Board A** — Daisy audio breakout. Mounted by **vertical slide-in rails** (drops down
  into guide slots, rests on stops, lid retains top). Front I/O: micro-USB + 2× 3.5 mm TRS
  (audio, MIDI), all **plate-mount jacks** (wall takes insertion force, not the board).
  Rear: one 2-wire Dupont pass-through.
- **Board B** — sensors. Screwed down onto **~15 mm bosses** w/ M3 heat-set inserts
  (tall to clear under-board wiring). Front: Cat6 cable pass-through (hard-wired, grommet).
  Rear: one 5-wire Dupont pass-through. **Corner holes are soldered — bosses go at free
  holes (positions TBD).**
- **Lid** — hinged at rear (stub-pivot), retained at front by cantilever snap + M3 screw.

---

## 2. Lid feature list (final)

1. **Rear stub-pivot hinge** — cylindrical pivot + conical lead-in tip; pins on lid
   flex-arms snapping into sockets in the box side walls. No loose pin.
2. **Front cantilever snap** — quick-hold / alignment only (non-load-bearing, PLA-friendly).
3. **Front M3 screw → heat-set insert** — primary front retention.
4. Anti-shear lugs — **dropped** (hinge pin + front screw already locate the lid in X/Y).
5. **Decorative text + line** — added in Bambu Studio (raised ~0.6–1 mm, color-swap layer).

---

## 3. Fusion 360 user-parameter table

Paste into **Modify → Change Parameters → +**. Expressions referencing other params
(e.g. `board_thickness + fdm_fit_gap`) are written out so Fusion computes them live.

### 3.1 Shell / global
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `wall_thickness` | 2.5 mm | CHOICE | 2.5–3 mm; must be < jack panel-thread depth |
| `floor_thickness` | 3 mm | CHOICE | stiffer floor carries the boss loads |
| `internal_height` | 35 mm | MEASURE | tallest stack above floor + ~5 mm clearance |
| `fdm_fit_gap` | 0.4 mm | CHOICE | running/snap fits (rails, pivot sockets, snap window) |
| `board_clearance` | 2.0 mm | CHOICE | board footprint vs walls (FDM warp allowance, per Formlabs) |
| `port_buffer` | 2.0 mm | CHOICE | extra around every panel cutout (FDM) |
| `ext_corner_fillet` | 2 mm | CHOICE | aesthetic outer-edge rounding |

### 3.2 Boards / compartments
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `board_thickness` | 1.6 mm | MEASURE | typical perfboard; confirm |
| `boardA_length` | 90 mm | MEASURE | from BREAKOUT.md §9 (~90×70); confirm |
| `boardA_width` | 70 mm | MEASURE | |
| `boardB_length` | 60 mm | MEASURE | smaller sensor board; confirm |
| `boardB_width` | 45 mm | MEASURE | |
| `divider_thickness` | 2.5 mm | CHOICE | full-height wall between compartments |

### 3.3 Board B bosses (screwed mounts)
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `boss_height` | 15 mm | CHOICE | under-board wiring clearance; lower others if only 1 spot needs it |
| `boss_diameter` | 11 mm | CHOICE | thick = stout (aspect ≈ 1.4:1); thickness is the main stability lever |
| `boss_pilot_dia` | 4.0 mm | MEASURE | M3 heat-set insert melt-hole — use insert datasheet |
| `boss_base_fillet` | 3 mm | CHOICE | fillet at floor joint — fixes the FDM base-crack stress riser |
| `bossB1_x` / `bossB1_y` | TBD | MEASURE | free-hole position from board corner (corners are soldered) |
| `bossB2_x` / `bossB2_y` | TBD | MEASURE | " |
| `bossB3_x` / `bossB3_y` | TBD | MEASURE | " (3 mounts may suffice on a small board) |

### 3.4 Board A slide rails
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `rail_slot_width` | `board_thickness + fdm_fit_gap` | EXPR | ≈ 2.0 mm channel |
| `rail_depth` | 3 mm | CHOICE | how far the channel grips each board edge |
| `rail_lead_in` | 1.5 mm | CHOICE | chamfered mouth at top so the board starts easily |
| `boardA_rest_height` | 5 mm | MEASURE | stop height; clears under-board solder tails |

### 3.5 Front panel cutouts
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `usb_cut_w` | TBD | MEASURE | Daisy USB connector/cable boot + `port_buffer` |
| `usb_cut_h` | TBD | MEASURE | |
| `usb_center_z` | TBD | MEASURE | USB height above board (sets vertical position) |
| `jack_hole_dia` | 6.4 mm | MEASURE | 3.5 mm panel-jack barrel Ø + clearance |
| `jack_spacing` | TBD | CHOICE | audio↔MIDI center-to-center (avoid nut overlap) |
| `jack_panel_max` | TBD | MEASURE | max panel thickness the jack threads accept (caps wall here) |
| `cat6_hole_dia` | 7 mm | MEASURE | cable jacket OD + grommet/relief |

### 3.6 Rear panel cutouts
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `dupont2_slot_w` | TBD | MEASURE | 1×2 housing width (≈ 5.1 mm) + clearance |
| `dupont2_slot_h` | TBD | MEASURE | housing height + clearance |
| `dupont5_slot_w` | TBD | MEASURE | 1×5 housing width (≈ 12.7 mm @ 2.54 pitch) + clearance |
| `dupont5_slot_h` | TBD | MEASURE | |

### 3.7 Stub-pivot hinge (rear)
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `pivot_dia` | 3.5 mm | CHOICE | **cylindrical** bearing section (not the cone) |
| `pivot_engagement` | 3.5 mm | CHOICE | depth into socket; deep enough to resist axial pop-out |
| `pivot_cone_chamfer` | 1.5 mm | CHOICE | 45° lead-in tip — assembly aid only |
| `socket_dia` | `pivot_dia + 0.3` | EXPR | running fit for rotation |
| `flexarm_length` | 12 mm | CHOICE | lid corner cantilever that flexes during snap-in |
| `flexarm_slot` | 1.5 mm | CHOICE | relief slot width defining the flex arm |
| `hinge_axis_offset` | TBD | CHOICE | pivot placement above/behind rear top edge (set when modeling) |

### 3.8 Front snap + screw
| Parameter | Value | Kind | Comment |
|---|---|---|---|
| `snap_arm_length` | 18 mm | CHOICE | long arm = low strain (PLA-friendly) |
| `snap_arm_root` | 2 mm | CHOICE | arm thickness at base |
| `snap_arm_width` | 6 mm | CHOICE | |
| `snap_hook` | 2 mm | CHOICE | hook protrusion |
| `snap_hook_depth` | 1.8 mm | CHOICE | barb thickness front-to-back |
| `snap_base_fillet` | 1 mm | CHOICE | rounded base (curve, not sharp corner) |
| `snap_window_clear` | 0.5 mm | CHOICE | catch window = hook + this |
| `lid_screw_clear` | 3.4 mm | CHOICE | M3 clearance hole through lid tab |
| `lid_insert_pilot` | 4.0 mm | MEASURE | M3 heat-set pilot in front-wall boss |
| `lid_boss_dia` | 8 mm | CHOICE | front-wall boss for the lid screw |

---

## 4. Outstanding measurements (caliper checklist)

- [ ] Board A & B: outline L×W, perfboard thickness
- [ ] Board B: free mounting-hole X/Y positions (corners are soldered)
- [ ] Board A: tallest stack height (socketed Daisy + headers); under-board lead length
- [ ] Board B: tallest module height; real under-board wiring clearance (is 15 mm right?)
- [ ] 3.5 mm jacks (×2): barrel Ø, thread length, nut OD/flats, body depth behind panel,
      anti-rotation flat/tab, max panel thickness
- [ ] Micro-USB: Daisy connector position vs board edge, height above board, plug-boot reach
- [ ] Cat6: cable jacket OD
- [ ] Dupont housings: 1×2 and 1×5 cross-section (W×H)

---

## 5. Material / process notes

- **PLA OK to start.** Heat-set inserts work fine in PLA (run iron ~190–210 °C). Caveats:
  brittle snaps (mitigated — front screw is the real retention, snap is non-critical) and
  heat creep (~55–60 °C softening; fine indoors, avoid sun/hot spots).
- **No living hinge in PLA** — would crack. Stub-pivot rotates around a bearing, no flex.
- **Print orientation:** snap arms and flex-arms should bend in the XY plane, not peel
  layers in Z (FDM loses ~50% elongation / 20–30% strength along layer lines).
- **Decorative top:** Bambu Studio Text tool, raised ~0.6–1 mm, filament/color-swap at the
  layer where text begins (or AMS paint). Print lid top-face-up.
- **Iterate the fits:** print a corner test coupon for snap window + pivot socket + rail
  slot before committing to the full box. FDM snap clearance has no universal value.
