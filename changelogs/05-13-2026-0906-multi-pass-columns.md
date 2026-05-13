# Multi-pass columns for long source lines

## Summary

Long source-code lines now use the full screen across multiple passes instead of
being truncated. When the falling head exits the bottom of the screen with
characters remaining, it wraps back to the top and continues overwriting cells
until the entire line has been placed. Only then does the normal fadeout begin.
This also fixes the pre-existing bug where columns using long lines would get
permanently stuck and never restart.

## Issues Resolved

- Columns would get stuck and never update when using long source lines as input.
- Only the first `trail_rows` characters (≤ 70 % of screen height) of each line
  were ever displayed; the rest were silently discarded.

## Root Causes

- `fade_threshold` was computed as `fast_threshold + line_length` (raw character
  count). For lines longer than the screen height, `dist` (bounded by screen
  height) could never exceed `fade_threshold`, so cell brightness never reached
  `0.0`, cells were never cleared, and the `restart` condition was never met.
- `trail_rows` was capped at `height × 0.7` but `line_length` was not, causing
  the mismatch.
- Cells from a previous pass that sat *below* the current head position had
  `dist = 0` (due to `.max(0)` clamping), making them falsely appear as "at the
  head" — causing rapid spinning and wrong rendering colour.

## Solutions

- Added `chars_placed: usize` to `Column` to track line consumption across passes.
- The placement loop skips cell placement once `chars_placed >= line_length`; the
  head continues its downward motion without touching cells.
- On head exit, if the line is not yet exhausted, `head_y` is reset to `-1.0`
  (one row above the top) instead of entering the fade/restart cycle.
- Changed `dist` calculation: cells *below* the current head position receive
  `dist = fast_threshold + 1` (settled trail) instead of `0`.
- Fixed `fade_threshold` to `fast_threshold + trail_rows` (screen-bounded) for
  all modes — this is the same formula that already worked correctly for short
  lines and random mode.
- Fading is now gated on `line_exhausted` (`chars_placed >= line_length`), so
  cells remain bright across passes and fade only after the full line is placed.
- `chars_placed` is reset to `0` in `Column::restart`.
- Updated an existing test that injected source chars without setting
  `line_length` to also set that field, keeping the test valid.

## Files Changed

- `src/main.rs` — Column struct, `new`, `tick`, `update_cells`, `restart`,
  `Rain::render`, and the `column_space_in_source_chars_does_not_place_cell` test.

## Status

Complete. All 28 tests pass.
