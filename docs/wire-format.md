# Wire Format (Tessellio)

## Update structure

An update is either:
- **Self-contained** (`base_update_id == 0`): establishes the whole surface.
- **Chained** (`base_update_id > 0`): continues the surface the named base established.

## Region

Each region is a rectangle of cells with an op stream (ROW/SPAN/FILL/END) inside a raw-DEFLATE envelope.

## Part

An update may be split into parts (MAX_PARTS_PER_UPDATE). Each part carries a subset of regions.

## Limits

- MAX_SURFACE_DIMENSION: 8192
- MAX_SURFACE_CELLS: 16,777,216
- MAX_UPDATE_WIRE_BYTES: 3,932,160
- MAX_REGIONS_PER_UPDATE: 64
- MAX_REGION_OPS_BYTES: 1,048,576
- MAX_REGION_WIRE_BYTES: 491,520
- MAX_PARTS_PER_UPDATE: 8
- DELTA_REFRESH_CELLS: 131,072
- REFRESH_BACKLOG_CELLS: 262,144
- JOURNAL_MAX_UPDATES: 240

See `src/format.rs` for all constants.
