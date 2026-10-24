# Wire Format (Tessellio)

## Update structure

An update is either:
- **Self-contained** (`update_base == 0`): establishes the whole surface.
- **Chained** (`update_base > 0`): continues the surface the named base established.

## Region

Each region is a rectangle of cells with an op stream (ROW/SPAN/FILL/END) inside a raw-DEFLATE envelope.

## Part

An update may be split into parts (MAX_PARTS_PER_UPDATE). Each part carries a subset of regions.

## Limits

- MAX_SURFACE_DIMENSION: 4096
- MAX_SURFACE_CELLS: 16,777,216
- MAX_UPDATE_WIRE_BYTES: 16 MiB
- MAX_REGIONS_PER_UPDATE: 256
- MAX_PARTS_PER_UPDATE: 16

See `src/format.rs` for all constants.
