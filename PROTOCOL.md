# Dell C1760nw: what the printer speaks

Found by probing a C1760nw on the local network, plus Dave Coffin's 2014
reverse engineering of HBPL v1 (dechifro.org/hbpl).

## Interfaces

| Port | What | Notes |
|------|------|-------|
| TCP 9100 | Raw print data, bidirectional | Jobs go here. `@PJL INFO ID` / `INFO STATUS` work. |
| TCP 515 | LPD, queue `lp` | Same data as 9100. |
| UDP 161 | SNMP v1, community `public` | Printer-MIB: status, toner levels, trays. |

There's no web UI, IPP, PostScript, or PCL. The Printer-MIB interpreter table
(`1.3.6.1.2.1.43.15`) lists one language: **HBPL** (Fuji Xerox Host Based
Printer Language, version 1). The host rasterizes and compresses every page.

Useful SNMP OIDs:

- `1.3.6.1.2.1.25.3.5.1.1.1`: hrPrinterStatus (3 idle, 4 printing, 5 warmup)
- `1.3.6.1.2.1.43.16.5.1.2.1.1`: front panel text
- `1.3.6.1.2.1.43.11.1.1.{6,8,9}.1.N`: supply N name / max / level
  (1 cyan, 2 magenta, 3 yellow, 4 black)

## Job layout

```
ESC%-12345X @PJL SET ... (job attributes, lines end in \n)
@PJL ENTER LANGUAGE=HBPL
'A' document header
  'C' page header ... 'Q' 'R' <image data> 'S' 'D'     (per page)
'B'
ESC%-12345X @PJL EOJ
```

Important PJL settings: `RENDERMODE=COLOR|GRAYSCALE`, `RESOLUTION=600`,
`BITSPERPIXEL=8`, and `JOBATTR="@MSIP=NORMAL"` (media type).

### Records and attributes

A record is one byte (`A`=0x41 begin doc, `B` end doc, `C` begin page,
`D` end page, `Q` end page attributes, `R` begin image, `S` end image).
`A`, `C` and `R` are followed by attributes: `<id> <type> <value>`, with
types:

| type | value |
|------|-------|
| 0xa1 | u8 |
| 0xa2 | u16 LE |
| 0xa4 | u32 LE |
| 0xc2 | two u16 (w, h) |
| 0xc3 | 4 bytes |
| 0xc4 | two u32 (w, h) |
| 0xb1 | byte array; next byte is the length's type (0xa2/0xa4), then the length, then the bytes |

Each attribute list ends with a fixed last attribute (0x83 for `A`, 0xa2 for
`C`, 0xa4 for `R`).

Page attributes that matter: 0x94 paper code/2 (letter 0, legal 1, A4 2,
255 custom), 0x95 custom size in 0.1 mm, 0x96 = 2 for custom size,
0x99 page number, 0x9a and 0xa2 raster width x height (width a multiple of
8), 0x9d 9 = mono or 139 = color, 0xa0 bits per pixel (8).

### Image data

48-byte header (starts `20 00 00 00`; byte 4 = 8 for mono, byte 6 = 1 for
color), with u32 stream lengths at offsets 32, 36, 40, 44. Then the streams
back to back: a token stream, then one delta stream per ink channel
(K, then C, M, Y for color). The last stream's length is implied.

Pixels are 8-bit ink amounts (255 = full), stored as K,C,M,Y. The printer
does its own screening, so the host sends contone.

### Compression

The pixels are one long array, row after row. At each position the encoder
tries to copy a run from one of five earlier positions:

    left (-1), up (-w), up-left (-w-1), up-right (-w+1), and up-right-2 (-w+2)
    (mono uses 8-left (-8) instead of up-right-2)

The five directions sit in a move-to-front list. Token *n* means "copy from
list entry *n*". If the previous run stopped on a mismatch, entry 0 can't
match, so token numbers shift down by one. Runs are 1 to 17057 pixels. If no
direction matches, the pixel is a literal (token 5), stored as the per-channel
difference from the pixel to its left in the delta streams.

Token codes (text table / image table):

| token | text | image |
|-------|------|-------|
| 0 | `0` | `10` |
| 5 (literal) | `10` | `0` |
| 1 | `110` | `110` |
| 2, 3, 4 | `11100`, `11101`, `11110` | same |
| 6 (switch table) | `111110` | same |

At row boundaries the encoder switches to the image table if the previous
row had more literals than repeated copies, and back again.

The run length follows each copy token:

    1: 00  2-3: 01x  4-5: 100x  6-9: 101xx  10-25: 110xxxx
    26-49: 111 0yy xxx (yy selects 26/34/42)   50-65: 111 011 xxxx
    66-97: 111 100 x5  98-161: 111 101 x6  162-673: 111 110 x9
    674-17057: 111 111 x14

Literal deltas: 0 `000`, +1 `001`, -1 `010`, then prefix + sign bit +
magnitude: 2-3 `011` s x, 4-7 `100` s xx, 8-15 `101` s xxx,
16-47 `110` s x5, 48-79 `1110` s x5, 80-128 `1111` s x6.

The token stream ends with `0xff`. Every stream is padded to a byte boundary
with 1 bits. The decoder's initial "row above" is all 0xff for color and all
0 for mono.

### Quirks copied from the reference encoder

- The first pixel of every row is forced to full ink (it's in the
  unprintable margin).
- For rows that contain any ink, pixels 96..width-96 are marked, and copies
  never cross between marked and unmarked pixels.
- An 8-pixel margin is blanked on every side by default.
