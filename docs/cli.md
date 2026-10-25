# The `tessellio-core` command-line tool

The tool is the screen codec's front door: it encodes a surface into the
codec's update, reads an update's bytes back, decodes them onto a canvas,
measures the codec against its budgets, and prints the codec's limits.

```sh
cargo build --release            # the library and the tool
./target/release/tessellio-core --help
```

## The grammar

```
tessellio-core <command> [flags]
```

| command | what it does |
|---|---|
| `encode` | encode a surface into the codec's update and print its bytes |
| `inspect` | read an update's bytes and print the envelope's fields and the regions' geometry |
| `decode` | decode an update's bytes and report the regions, the cells and the surface |
| `bench` | run the measured performance scenarios and print the medians and the p95s |
| `limits` | print the codec's limits, as a table and as JSON |

Every command accepts `--json`, and every command's `--help` prints its
flags and a worked example.

## The exit status

The status is a contract, and it is the same for every command:

| status | meaning |
|---|---|
| `0` | the command succeeded |
| `1` | the check failed — a measurement past its budget, a verdict that does not match |
| `2` | the usage was wrong — an unknown verb, a missing or unknown flag |
| `3` | the input could not be read — bad hex, an update that does not decode |

A refusal is a typed error with a named reason, on standard error, in the
form `reason: message`:

```
$ tessellio-core inspect --hex zz
input: the hex text holds 'z', which is not a hexadecimal digit
```

No input makes the tool panic. A bad hex, a truncated envelope, an update
whose base the receiver does not hold, a region that does not decode: each is
a refusal with its own reason.

## Input and output

| direction | form |
|---|---|
| bytes on the wire | hexadecimal, lower case, `:` and whitespace ignored |
| bytes in a file | hexadecimal text, one part per line for `decode` |
| bytes from a terminal | hexadecimal text on standard input |
| the machine contract | `--json`, the shapes below |
| the human contract | a table, for a person reading a terminal |

The tool never writes a temporary file.

## The JSON shapes

Every JSON document is one object. Its keys are the contract; the human
table is not. The shapes below are pinned by `tests/cli_tool.rs`, so a change
to one is a change to the contract.

### `encode`

```json
{
  "command": "encode",
  "update": {
    "update_id": 1,
    "base_update_id": 0,
    "kind": "self-contained",
    "width": 64,
    "height": 48,
    "changed_cells": 3072,
    "sent_cells": 3072,
    "backlog_cells": 0,
    "payload_bytes": 1234,
    "regions": 1,
    "reset_reasons": ["first"]
  },
  "parts": [
    {
      "part_index": 0,
      "part_count": 1,
      "update_bytes": 1234,
      "regions": 1,
      "payload_bytes": 1234,
      "bytes_len": 1240,
      "bytes_hex": "0802…"
    }
  ],
  "regions": [
    {
      "index": 0,
      "x": 0,
      "y": 0,
      "width": 64,
      "height": 48,
      "cells": 3072,
      "payload_bytes": 1234,
      "payload_sha256": "…",
      "ops": { "rows": 48, "spans": 0, "fills": 48, "cells": 3072 }
    }
  ],
  "phases": [
    { "phase": "convert", "count": 1, "total_ms": 1.2, "median_ms": 1.2, "p95_ms": 1.2 }
  ],
  "encode_ms": 3.4
}
```

`kind` is `self-contained`, `chained` or `empty`. `phases` names the
encoder's own phases: `convert`, `compare`, `spans`, `pack`, `compress`,
`reference`. A region's `ops` is `null` when its op stream does not decode.

### `inspect`

```json
{
  "command": "inspect",
  "envelope": {
    "update_id": 1,
    "base_update_id": 0,
    "width": 64,
    "height": 48,
    "part_index": 0,
    "part_count": 1,
    "update_bytes": 1234,
    "codec": 8,
    "codec_name": "tessellio",
    "regions": 1
  },
  "regions": [
    {
      "index": 0,
      "x": 0,
      "y": 0,
      "width": 64,
      "height": 48,
      "cells": 3072,
      "payload_bytes": 1234,
      "payload_sha256": "…"
    }
  ],
  "bytes_len": 1240
}
```

### `decode`

```json
{
  "command": "decode",
  "parts": 1,
  "applied_updates": [1],
  "pending": false,
  "rejection": null,
  "regions": [ …the regions it painted… ],
  "cells_painted": 3072,
  "surface": {
    "width": 64,
    "height": 48,
    "applied_update_id": 1,
    "cells": 3072,
    "sha256": "…"
  }
}
```

`rejection` is the codec's reason when the parts do not assemble — `PART_GAP`,
`INCOMPLETE_UPDATE`, `DECODE_ERROR`, `BASE_MISMATCH`, `GEOMETRY_MISMATCH`,
`UNKNOWN_CODEC` — and `null` otherwise. `surface` is the canvas the update
was applied to; it is `null` when the update carried no surface.

### `bench`

```json
{
  "command": "bench",
  "profile": "optimized",
  "iterations": 5,
  "scenarios": [
    {
      "name": "pointer_1px",
      "summary": "a one-cell pointer move over an office scene",
      "bytes": 92,
      "byte_budget": 128,
      "encode_ms_median": 0.79,
      "encode_ms_p95": 0.92,
      "baseline_ms_median": 1.43,
      "encode_over_baseline": 0.55,
      "encode_ms_budget": 40.0,
      "regions": 1,
      "parts": 1,
      "within_budget": true
    }
  ],
  "summary": { "total": 9, "failed": 0 }
}
```

`profile` is `optimized` or `unoptimized`: the tool knows which build it
is, and the time budget it holds itself to is the one for that profile.

### `limits`

```json
{
  "command": "limits",
  "codec": "tessellio",
  "wire_id": 8,
  "groups": [
    {
      "name": "surface",
      "spec": "§7",
      "entries": [
        {
          "name": "MAX_SURFACE_DIMENSION",
          "value": 8192,
          "unit": "cells",
          "spec": "§7"
        }
      ]
    }
  ]
}
```

The groups are `identity`, `surface`, `regions`, `parts`, `updates`,
`journal`, `timing` and `relations`. Every numeric cap is the crate's own
constant, so the table cannot drift from the code that enforces it.

## The budgets

The bench holds the codec to two budgets, and both are in
[`tessellio_core::measured`](https://docs.rs/tessellio-core):

* the **byte** budget is the reference implementation's measured bound for
  that scenario — 92 bytes for a one-cell pointer move, 2 466 for a 600×40
  retyped block, 82 799 for a 1080p self-contained update, and so on. The
  bench reproduces those measurements exactly, on the crate's own scenes, and
  the budget is machine-independent;
* the **time** budget is the specification's measured cost of an encode —
  ≈ 4 ms for a chained update and 7.53 ms for a 1080p self-contained one —
  with an order of magnitude of slack, so a shared machine does not fail the
  gate and a regression of an order of magnitude does. The budget is stated
  for an optimized build; an unoptimized build is an order of magnitude
  slower, and the tool knows which one it is.

The bench also prints the encode's cost over the capture interface's own
conversion of the same frame (`encode_over_baseline`), measured in the same
run: the conversion is the same work on every machine, so the ratio says how
much the codec costs on top of what the capture already paid.

A scenario that leaves its budget makes the command exit with the status
`1`, so `tessellio-core bench` is a gate.

## The digests

`payload_sha256` is the SHA-256 of a region's payload, and `surface.sha256`
is the SHA-256 of the surface's cells, each cell as two little-endian bytes.
Both are 64 lower-case hexadecimal digits. The tool computes SHA-256 itself,
so it has no dependency that could change what it prints.

## The envelope

An update travels as parts, and each part is the schema's `Update`
([`tessellio-proto`](https://github.com/tessellio/tessellio-proto)): the field
numbers are that schema's normative table, and the encoding is its canonical
one — a field equal to its default is omitted.
