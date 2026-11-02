# /// script
# requires-python = ">=3.10"
# dependencies = ["capstone==5.0.9"]
# ///
"""Regression tests for static CLPC report discovery; run with uv run."""

import struct
import unittest

from clpc import rust_table
from clpc_binary import Flow, labels


class Code:
    def __init__(self, words):
        self.data = struct.pack(f"<{len(words)}I", *words)

    def offset_for(self, address):
        return address - 0x1000


class DiscoveryTests(unittest.TestCase):
    def test_traces_double_cumulative_report_without_integer_rounding(self):
        image = Code([
            0xD2840001,  # mov x1, #0x2000
            0xFD400000,  # ldr d0, [x0]
            0xFD400021,  # ldr d1, [x1]
            0x1E612800,  # fadd d0, d0, d1
            0xFD000020,  # str d0, [x1]
            0xD65F03C0,  # ret
        ])
        flow = Flow(image, 0x1000, 0x1000 + len(image.data), ["AVE"], 0, reports=[0x2000])
        flow.run()
        self.assertFalse(flow.unknown)
        self.assertEqual(labels(next(iter(flow.report_writes.values()))), {"AVE", "counter:2000"})

    def trace(self, words):
        image = Code(words)
        flow = Flow(image, 0x1000, 0x1000 + len(image.data), ["DRAM", "DCS"], 0)
        fields, _ = flow.run()
        self.assertFalse(flow.unknown)
        return {offset: labels(value) for offset, value in fields.items()}

    def test_single_lane_load_preserves_the_other_float(self):
        fields = self.trace([
            0xBD400001,  # ldr s1, [x0]
            0x91002009,  # add x9, x0, #8
            0x0D409121,  # ld1 {v1.s}[1], [x9]
            0xFD000101,  # str d1, [x8]
            0xD65F03C0,
        ])
        self.assertEqual(fields, {0: {"DRAM"}, 4: {"DCS"}})

    def test_widened_packed_floats_write_two_independent_energy_counters(self):
        image = Code([
            0xBD400000,  # ldr s0, [x0]
            0xBD400801,  # ldr s1, [x0, #8]
            0x2D0007E0,  # stp s0, s1, [sp]
            0xFD4003E0,  # ldr d0, [sp]
            0x0E617800,  # fcvtl v0.2d, v0.2s
            0xD2840001,  # mov x1, #0x2000
            0x3DC00021,  # ldr q1, [x1]
            0x4E61D400,  # fadd v0.2d, v0.2d, v1.2d
            0x3D800020,  # str q0, [x1]
            0xD65F03C0,
        ])
        flow = Flow(image, 0x1000, 0x1000 + len(image.data), ["ISP", "AVE"], 0, reports=[0x2000, 0x2008])
        flow.run()
        self.assertFalse(flow.unknown)
        actual = {address: labels(value) for (_, address), value in flow.report_writes.items()}
        self.assertEqual(actual, {0x2000: {"ISP", "counter:2000"}, 0x2008: {"AVE", "counter:2008"}})

    def test_integer_copy_preserves_separate_packed_float_fields(self):
        fields = self.trace([
            0xBD400000,  # ldr s0, [x0]: DRAM
            0xBD400801,  # ldr s1, [x0, #8]: DCS
            0x2D0007E0,  # stp s0, s1, [sp]
            0xF94003E9,  # ldr x9, [sp]
            0xF9000109,  # str x9, [x8]
            0xD65F03C0,  # ret
        ])
        self.assertEqual(fields, {0: {"DRAM"}, 4: {"DCS"}})

    def test_simd_add_and_duplicate_preserve_independent_lanes(self):
        fields = self.trace([
            0xBD400000,  # ldr s0, [x0]: DRAM
            0xBD400801,  # ldr s1, [x0, #8]: DCS
            0x2D0007E0,  # stp s0, s1, [sp]
            0xFD4003E0,  # ldr d0, [sp]
            0x0E040421,  # dup v1.2s, v1.s[0]
            0x0E21D400,  # fadd v0.2s, v0.2s, v1.2s
            0xFD000100,  # str d0, [x8]
            0xD65F03C0,  # ret
        ])
        self.assertEqual(fields, {0: {"DRAM", "DCS"}, 4: {"DCS"}})

    def test_rust_export_requires_memory_and_package_counters_and_preserves_their_ids(self):
        driver = {
            "bundle": "example",
            "channels": {
                component: {"id": f"0x{index:08x}12345678", "dependencies": [component]}
                for component, index in (
                    ("CPU", 16), ("GPU", 25), ("ANE", 24),
                    ("RAM", 26), ("DCS", 27), ("CHIP", 9),
                )
            },
        }
        catalog = {"complete": True, "macos": "27.2", "build": "26B123", "drivers": [driver]}
        output = rust_table(catalog)
        self.assertIn("ram: 26", output)
        self.assertIn("ram: 0x1234_5678", output)
        self.assertIn("dcs: 27", output)
        self.assertIn("chip: 9", output)
        self.assertNotIn("gpusram:", output)
        self.assertIn("vdec: None", output)
        driver["channels"]["AVE"] = {"id": "0x0000003b12345678", "unit": "mJ"}
        output = rust_table(catalog)
        self.assertIn('ave: (59, "mJ")', output)
        self.assertIn('ave: Some(0x1234_5678)', output)
        for channel in (
            {"id": "0x0000003c12345678", "unit": "mJ"},
            {"id": "0x0000003b12345678", "unit": "nJ"},
        ):
            catalog["drivers"].append({
                "bundle": "other",
                "channels": {**driver["channels"], "AVE": channel},
            })
            with self.assertRaises(ValueError):
                rust_table(catalog)
            catalog["drivers"].pop()
        del driver["channels"]["RAM"]
        with self.assertRaises(ValueError):
            rust_table(catalog)

    def test_duplicate_uses_selected_lane_and_64_bit_store_stops_after_two_lanes(self):
        fields = self.trace([
            0xBD400000,  # ldr s0, [x0]: DRAM
            0xBD400801,  # ldr s1, [x0, #8]: DCS
            0x2D0007E0,  # stp s0, s1, [sp]
            0xFD4003E0,  # ldr d0, [sp]
            0x0E0C0401,  # dup v1.2s, v0.s[1]
            0xFD000101,  # str d1, [x8]
            0xD65F03C0,  # ret
        ])
        self.assertEqual(fields, {0: {"DCS"}, 4: {"DCS"}})

    def test_128_bit_copy_preserves_all_four_lanes(self):
        fields = self.trace([
            0x3DC00000,  # ldr q0, [x0]
            0x3D800100,  # str q0, [x8]
            0xD65F03C0,  # ret
        ])
        self.assertEqual(fields, {0: {"DRAM"}, 4: {"DRAM"}, 8: {"DCS"}, 12: {"DCS"}})


if __name__ == "__main__":
    unittest.main()
