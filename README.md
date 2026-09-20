# gtfs-cleaner

Removes invalid trips from a GTFS feed.

Some feeds (for example the [IDFM](https://eu.ftp.opendatasoft.com/stif/GTFS/IDFM-gtfs.zip)
feed for Île-de-France) contain trips whose last `stop_times.txt` row has no
`departure_time`. Many GTFS consumers reject them. `gtfs-cleaner` detects
these trips and removes them from the archive.

## What it does

1. Reads `stop_times.txt` and, for each `trip_id`, looks at its last row
   (in file order). If `departure_time` is empty, the trip is marked invalid.
2. Removes every row that references an invalid trip from:
   - `stop_times.txt`
   - `trips.txt`
   - `frequencies.txt` (if present)
3. Copies every other file in the archive unchanged (raw copy, no
   recompression).
4. Writes the result to `<input>.cleaned.zip` next to the input file.

Filtering and compression are parallelised (rayon + parallel deflate), so
large feeds are processed quickly.

## Installation

### Prebuilt binary

Each tagged release ships a statically linked Linux binary
(`x86_64-unknown-linux-musl`), with no runtime dependencies:

```sh
tar -xzf gtfs-cleaner-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz
sha256sum -c gtfs-cleaner-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz.sha256
```

### From source

Requires a recent stable Rust toolchain and the musl target:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl --bin gtfs-cleaner
```

The binary is written to `target/x86_64-unknown-linux-musl/release/gtfs-cleaner`.

## Usage

```sh
gtfs-cleaner IDFM-gtfs.zip
# -> IDFM-gtfs.cleaned.zip
```

The tool prints the number of invalid trips and, for each filtered file, the
number of rows kept and removed.

The output is first written to `<output>.tmp`, then renamed, so an
interrupted run never leaves a truncated `.cleaned.zip` behind.

## Limitations

- Input files must be UTF-8 (a leading BOM is accepted).
- The "last stop" of a trip is the last row for that `trip_id` in
  `stop_times.txt`, not the row with the highest `stop_sequence`.
- Filtered files are written without Zip64: each one must stay below 4 GiB
  once decompressed.

## Development

```sh
make fetch   # download the IDFM feed
make build   # static musl release build
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```
