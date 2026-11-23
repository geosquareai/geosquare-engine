# Concepts

This page explains the words used in the engine. Read it first.

## What a GID is

A GID is a short text code for one grid cell. Example: `2` is one cell at level 1. The code is built one character at a time, from the outside in. Each character picks one child inside its parent.

The GID length is the level. A GID of length 9 is a level 9 cell.

The root cell has level 0. Its GID is the empty string `""`.

## Levels and cell size

Each level splits the cell above it into smaller cells. The split is 5 by 5 at some levels and 2 by 2 at others. The split always alternates in the same order, starting at level 0.

| Level | Cells per side | Side length |
|---|---|---|
| 0 | 1 | 50,000 km |
| 1 | 5 | 10,000 km |
| 2 | 10 | 5,000 km |
| 3 | 50 | 1,000 km |
| 4 | 100 | 500 km |
| 5 | 500 | 100 km |
| 6 | 1,000 | 50 km |
| 7 | 5,000 | 10 km |
| 8 | 10,000 | 5 km |
| 9 | 50,000 | 1 km |
| 10 | 100,000 | 500 m |
| 11 | 500,000 | 100 m |
| 12 | 1,000,000 | 50 m |
| 13 | 5,000,000 | 10 m |
| 14 | 10,000,000 | 5 m |

Level 14 is the finest level. Its cells are 5 m wide.

## Domains

A domain is one country or region. Each domain has its own grid frame. The same GID text can mean different places in different domains. A domain is always named when a geographic answer is needed.

| Domain ID | Code | Status |
|---|---|---|
| 1 | ID | Released |
| 2 | VN | Candidate |
| 3 | BN | Released |
| 4 | KH | Candidate |
| 5 | LA | Released |
| 6 | MY | Released |
| 7 | MM | Candidate |
| 8 | PH | Released |
| 9 | SG | Released |
| 10 | TH | Candidate |
| 11 | TL | Candidate |

Released means the domain is supported under GeoSquare's support policy. Candidate means the domain works only when the caller opts in. Pass `allow_candidate_crs=True` to opt in. Released is not official national certification. It is GeoSquare's support decision.

## Packed IDs

A packed ID puts the domain ID and the GID into one signed 64-bit integer. Use `pack` and `unpack` to convert. Domain IDs must be between 1 and 511.

## Errors

The engine never repairs input. Invalid input raises `ValueError`. Examples:

- a GID with a character that does not exist at its level;
- a level outside 0 to 14;
- a domain ID outside 1 to 511;
- a request for children of a level 14 cell.

Check the error. Fix the input. Do not catch the error and guess a value.

## Where the rules come from

The rules live in the spec, in the `geosquare-v2` repository. This engine implements them in Rust. Each binding must pass the same test vectors in `vectors/spec-2.0/`.
