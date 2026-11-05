# CLPC energy counters from macOS firmware

Maintainer tool: [scripts/clpc.py](../scripts/clpc.py) downloads Apple restore-image components, discovers CPU/GPU/ANE/DRAM/DCS and package aggregate report IDs for every included CLPC driver, and prints Rust constants ready to paste into [src/clpc.rs](../src/clpc.rs). [scripts/clpc_binary.py](../scripts/clpc_binary.py) parses Mach-O tables and traces the recognized ARM64 producers. Neither script connects to another Mac or needs root.

## Run

Requires macOS, Xcode command-line tools (`nm`), `curl`, and `uv`. The script declares its Capstone dependency. Run these commands from the repository root.

```sh
uv run scripts/clpc.py scan latest
uv run scripts/clpc.py scan 26.6.2
uv run scripts/clpc.py scan 25A8364
uv run scripts/clpc.py history 26
uv run scripts/clpc.py compare \
  out/clpc-static/catalog-26.6.2-25G83.json \
  out/clpc-static/catalog-27.0.1-26A434.json
```

The runtime currently selects CLPC mappings only on macOS 27.x; other macOS versions use legacy sources. The extractor can also analyze older releases, but this does not enable CLPC on those releases.

Each `scan` analyzes one IPSW build and every CLPC driver found in its referenced kernelcaches. `latest` resolves through Apple's catalog rather than selecting a hardcoded build. A generated table establishes mappings for that build; compare other builds before treating the table as compatible with an entire macOS major version.

`scan` also accepts a direct Apple IPSW URL, including a beta image. An ambiguous version such as 26.0.1 requires its exact build. `history` runs the same analysis for every indexed stable IPSW, including hardware-specific releases, and compares the results; it does not enumerate betas or OTA-only releases. Unresolved counters are reported as unknown, not unchanged.

Latest releases come from [Apple’s IPSW catalog](https://mesu.apple.com/assets/macos/com_apple_macOSIPSW/com_apple_macOSIPSW.xml); historical URLs come from the [IPSW.me API](https://api.ipsw.me/), combining all Mac models. Downloads always come from Apple’s HTTPS CDN. HTTP ranges fetch only `BuildManifest.plist` and its referenced kernelcaches, which are LZFSE-decompressed locally.

`scan` prints `INDICES_<major>` and `CLPC_KEYS` directly to stdout; copy these constants into [src/clpc.rs](../src/clpc.rs). Progress and the discovery summary go to stderr. Rust output requires a complete scan and uniform table indices across the drivers. The script creates no `.rs` file and never edits Rust sources.

All downloads, full report tables, producer traces, JSON catalogs and TSV summaries go to the ignored `out/clpc-static/` at the repository root. The manifest version/build, image hashes, driver UUIDs and code/data hashes identify the exact analyzed inputs.

## Method

A kernelcache can contain several CLPC implementations. Each is selected by its bundle ID and own symbol table; marketing chip names are not the lookup key.

The names table contains opaque names and low 32-bit identifiers. The actions table links entries to storage and report-generation functions. The complete IOReport ID is `(table_index << 32) | low_identifier`; on-disk index fields can still be zero because the driver initializes them at startup.

The recognizer follows readable PMGR node names (`GPU`, `ANE`, `GPUSRAM`, `DRAM`, `DCS`) and ACC CPU inputs through `updateMetrics` to rounded cumulative scalar writes, then joins storage to the actions table. It handles both inline calculations and the separate component helper. Sample layouts come from sampler call arguments; bounded loops preserve separate node identities. Packed float copies preserve individual lanes even when the compiler uses integer registers. Ultra aggregation happens inside the inspected drivers.

- `identified`: all six core producers were recognized by the bounded ARM64 trace. DRAM and DCS are identified separately and summed by the sampler. The package aggregate must depend on CPU and every PMGR node found for that driver. GPU follows the explicit firmware sum, including GPUSRAM where the firmware adds it. Separate AVE, VDEC, and ISP reports are selected independently; unresolved components remain explicitly listed and export as missing IDs, not guessed counters. GPUSRAM remains an input to tracing GPU sums, but is not searched for or exported as a separate component.
- `needs_review`: an assignment is missing or ambiguous. Inspect the saved raw reports, candidates and trace; do not substitute a guessed index. Incomplete scans exit with status 2 and leave stdout empty.

The analyzer contains no precomputed chip keys and does not read Rust mappings or previous scan results to identify counters. For the DirectAccessEnergySampler CPU path, it tracks the sampler's index words into energy/time records and verifies that `updateMetrics` reads those same records. Report IDs, opaque names, table indices and storage addresses all come from the analyzed firmware; unfamiliar layouts remain unresolved.

## Established results

- 27.0 (`26A428`) and 27.0.1 (`26A434`): identical 51 selected IDs across 17 implementations; all four per-driver code/data segment hashes also match.
- All 16 indexed stable macOS 26 images through 26.6.2: 216 driver appearances, 200 comparisons, no selected ID changes. Driver UUIDs and executable bytes do change in some minor and patch releases.
- 26 → 27: the shared drivers retain CPU index 16; GPU moves 24 → 25 and ANE 23 → 24. Low identifiers remain unchanged. These are observed layouts, not a rule for future releases.
- A changed UUID does not necessarily mean changed IDs. History compares each driver with its own previous appearance; absence from a hardware-specific image is not treated as removal.
- 27.2 (`26B123`): all 17 implementations expose separate DRAM and DCS cumulative reports at indices 26 and 27. No combined RAM report was found in this catalog. CPU/GPU/ANE IDs remain unchanged.

For a new firmware, run `scan`, review its statuses and compare catalogs, then update macmon’s runtime mapping separately. Runtime obtains the live `DriverID` from IORegistry and requests full IDs through IOReport; binary storage addresses are not sampler inputs. Static extraction does not establish access permissions or measurement accuracy: verify on hardware under separate loads against an independent reference.
