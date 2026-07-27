# Multi-monitor / hot-swap support

## Goal

The pet moves **freely to any monitor** (dragged across the seam), with **no
pinning**, and survives monitor **hot-swap** (plug/unplug) gracefully — no
full-renderer rebuild, never vanishes.

## The Wayland constraint

A layer surface is bound to one output at creation and **cannot span or move
between outputs**. So "any monitor" means **one mascot surface per output**,
all mapped, with the pet drawn only on the surface whose output contains the
pet's **global** position; the others stay empty and click-through. During a
drag, Wayland's implicit pointer grab keeps delivering motion to the pressed
surface even over other outputs, so the pet hands off automatically as its
global position crosses a boundary — visually gliding across because the
surfaces tile the whole desktop.

## Model change: position becomes global

- `Position` moves from output-local `{output_name, margin_x, margin_y}` to a
  **global logical point** `{x, y}` (+ `visible`), in the compositor's
  multi-output logical coordinate space.
- Each output has a logical rect `{x, y, w, h}` from xdg-output (`OutputInfo`).
- Resolve per frame: the pet's **active output** = the rect containing the
  pet's reference point (its centre); local margins = `global - output.origin`.
- **Back-compat:** migrate old single-position files — treat the stored margins
  as local to the recorded `output_name` (or the first output) and add its
  origin to get a global point.

## Phases (each independently testable)

### Phase 1 — global position + output targeting + throw-to-monitor
- **1a** `surface/outputs.rs`: pure `OutputRect {name,x,y,w,h}` model + helpers
  `output_at(point)`, `nearest_output(point)`, `clamp_into(rect, size)`. Tested.
- **1b** `Position` global model + migration; pure `global↔local` conversion. Tested.
- ~~**1c** Create the mascot surface on the resolved output.~~ **Skipped:**
  needs the same "create surfaces once output info arrives" restructuring 2a
  does anyway — done once, there.
- ~~**1d** Recreate the surface on the drop output (throw-to-monitor).~~
  **Skipped:** the destroy/recreate lifecycle is the riskiest code in the
  drag path and 2a deletes it wholesale; throw-to-monitor falls out of 2a
  for free (release resolves the active output, which just switches).

### Phase 2 — seamless cross-boundary drag
- **2a** `SurfaceSet`: one `MascotSurface` per output (map), all mapped; pet
  drawn on the active one, others blank + click-through. Lands in two steps:
  **2a-i** mechanical lifecycle refactor (surfaces created from the output
  handlers, behavior-identical on one monitor), **2a-ii** blank click-through
  inactive surfaces + drop-resolves-output (throw-to-monitor).
- **2b** Drag hand-off: grab stays on the press surface; when the global centre
  crosses an output boundary, switch which surface draws the pet. (Straddle
  clipping deferred — snap when the centre crosses.)

### Phase 3 — graceful hot-swap
- **3a** `new_output` → spin up a surface; `output_destroyed` → tear it down and,
  if it held the pet, relocate to the nearest remaining output (clamp global
  into it). No full rebuild.
- **3b** Drop the `closed → error → supervisor-rebuild` path for output loss.

## Files touched
- new `surface/outputs.rs` (pure geometry)
- `surface/position.rs` (global model + migration)
- `surface/mascot.rs` (per-output surface; `SurfaceSet` manager)
- `app.rs` (render / drag / output handlers use global position + active surface)
- `wayland/globals.rs` (`OutputHandler` drives the surface set)

## Tests
- **Pure:** `output_at` / `nearest_output` / `clamp_into`; global↔local; Position
  migration; drag-release output resolution.
- **Live:** single-monitor unchanged; throw to a 2nd monitor; drag glide across
  the seam; unplug/replug without vanish or rebuild.

## Risk
The surface layer + drag path is exactly where the earlier drag regression
lived. 1a/1b are pure and safe; 1c/1d and phase 2 touch surfaces — land each
increment behind its own test before the next.
