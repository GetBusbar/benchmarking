"""A real field snapshot split into per-cell pieces, as per-cell boxes would write them, merges back to
the same matrix: every cell byte-for-byte, every gateway-level field unchanged."""
import copy
import json
from pathlib import Path
import unittest

import merge_cells
from merge_cells import MergeError, merge, full_grid
import per_cell

ROOT = Path(__file__).resolve().parent.parent
BUSBAR = ROOT / 'results/snapshots/result_busbar_2026-11-09T05-55-33Z.json'
BIFROST = ROOT / 'results/snapshots/result_bifrost_2026-11-09T05-55-20Z.json'


def split(snap, units):
    """What a box running OTB_CELLS=<unit cells> writes: the same snapshot narrowed to its cells, with
    column/matrix `served` recomputed by the engine's rule and its own box's qualification reading."""
    pieces = []
    for n, unit in enumerate(units):
        piece = copy.deepcopy({k: v for k, v in snap.items() if k != 'matrix'})
        piece['matrix'] = copy.deepcopy({k: v for k, v in snap['matrix'].items() if k != 'upstreams'})
        ups = {}
        for egress, up in snap['matrix']['upstreams'].items():
            cells = {i: copy.deepcopy(c) for i, c in up['cells'].items() if f'{i}>{egress}' in unit}
            if cells:
                col = copy.deepcopy({k: v for k, v in up.items() if k != 'cells'})
                col['cells'] = cells
                col['served'] = any(c.get('served') is True for c in cells.values())
                ups[egress] = col
        piece['matrix']['upstreams'] = ups
        piece['matrix']['served'] = any(c['served'] for c in ups.values())
        stamp = f'2026-11-09T06:{n // 60:02d}:{n % 60:02d}Z'
        piece['measured_at'] = piece['matrix']['measured_at'] = stamp
        q = piece['rig']['box_qualify']
        q['observed_rps'] = q['observed_rps'] + n
        q['drift_pct'] = (q['observed_rps'] - q['baseline_rps']) / q['baseline_rps'] * 100
        piece['matrix']['rig'] = copy.deepcopy(piece['rig'])
        pieces.append((f'cells/{n}/result_{snap["gateway"]}_{stamp}.json', piece,
                       {'instance_id': f'i-{n:017x}', 'cells': unit}, f'{n:064x}'))
    return pieces


def cell_bytes(snap):
    return {f'{i}>{e}': json.dumps(c) for e, up in snap['matrix']['upstreams'].items() for i, c in up['cells'].items()}


class MergeRealSnapshot(unittest.TestCase):
    def setUp(self):
        self.snap = json.loads(BUSBAR.read_text())
        self.manifest = per_cell.load_manifest(ROOT, 'busbar')
        self.units = [u['cells'] for u in per_cell.units_for('busbar', self.manifest)]

    def test_one_box_per_cell_reproduces_every_cell_byte_for_byte(self):
        self.assertEqual(len(self.units), 36)
        merged = merge(split(self.snap, self.units), full_grid())
        self.assertEqual(cell_bytes(merged), cell_bytes(self.snap))
        for egress, up in self.snap['matrix']['upstreams'].items():
            got = merged['matrix']['upstreams'][egress]
            self.assertEqual({k: v for k, v in got.items() if k != 'cells'}, {k: v for k, v in up.items() if k != 'cells'})

    def test_gateway_level_fields_are_the_engines_and_per_box_readings_are_kept(self):
        pieces = split(self.snap, self.units)
        merged = merge(pieces, full_grid())
        for key in ('schema_version', 'definitions', 'gateway', 'build', 'arch', 'hardware', 'config'):
            self.assertEqual(merged[key], self.snap[key], key)
        for key, v in self.snap['matrix'].items():
            if key not in ('upstreams', 'rig', 'measured_at'):
                self.assertEqual(merged['matrix'][key], v, key)
        for key in ('arch', 'release_url', 'engine', 'mock', 'ugen'):
            self.assertEqual(merged['rig'][key], self.snap['rig'][key])
        q = merged['rig']['box_qualify']
        self.assertEqual((q['outcome'], q['baseline_rps'], q['band_pct'], q['boxes']), ('pass', 96619.0, 20.0, 36))
        self.assertNotIn('observed_rps', q)
        self.assertEqual(merged['measured_at'], max(p[1]['measured_at'] for p in pieces))
        self.assertEqual(merged['matrix']['rig'], merged['rig'])
        self.assertEqual(len(merged['cell_runs']), 36)
        run = next(r for r in merged['cell_runs'] if r['cells'] == ['openai>openai'])
        self.assertTrue(run['box']['instance_id'].startswith('i-'))
        self.assertIn('observed_rps', run['rig']['box_qualify'])
        self.assertEqual(merged['placement']['mode'], 'per-cell')

    def test_merged_output_is_a_full_run_to_the_site(self):
        merged = merge(split(self.snap, self.units), full_grid())
        self.assertEqual(sorted(merge_cells.coords(merged)), sorted(merge_cells.coords(self.snap)))
        self.assertEqual(merge_cells.output_name(merged), 'result_busbar_2026-11-09T06-00-35Z.json')


class MergeWithUnsupportedCells(unittest.TestCase):
    def test_unprobed_cells_ride_on_the_first_box_and_come_back_as_the_engine_wrote_them(self):
        snap = json.loads(BIFROST.read_text())
        manifest = per_cell.load_manifest(ROOT, 'bifrost')
        measured, unsupported = per_cell.classify(manifest)
        states = {f'{i}>{e}': c['served'] for e, up in snap['matrix']['upstreams'].items() for i, c in up['cells'].items()}
        self.assertEqual(sorted(unsupported), sorted(c for c, s in states.items() if s in ('not_configurable', 'untestable')))
        units = [u['cells'] for u in per_cell.units_for('bifrost', manifest)]
        self.assertEqual(len(units), len(measured))
        self.assertEqual(len(units[0]), 1 + len(unsupported))
        merged = merge(split(snap, units), full_grid())
        self.assertEqual(cell_bytes(merged), cell_bytes(snap))
        # A column is served when any of its cells was (record_upstream_cell). This c4965b2b snapshot
        # predates that rule for its all-untestable bedrock column, so compare against the rule.
        for egress, up in snap['matrix']['upstreams'].items():
            want = any(c.get('served') is True for c in up['cells'].values())
            self.assertEqual(merged['matrix']['upstreams'][egress]['served'], want, egress)


class Refusals(unittest.TestCase):
    def setUp(self):
        self.snap = json.loads(BUSBAR.read_text())
        self.units = [[f'{i}>{e}'] for e in merge_cells.DIALECTS for i in merge_cells.DIALECTS]

    def test_a_cell_measured_twice_is_refused(self):
        pieces = split(self.snap, self.units + [['openai>openai']])
        with self.assertRaisesRegex(MergeError, 'openai>openai is in both'):
            merge(pieces, full_grid())

    def test_a_missing_cell_is_refused_unless_partial(self):
        pieces = split(self.snap, self.units[1:])
        with self.assertRaisesRegex(MergeError, 'missing'):
            merge(pieces, full_grid())
        self.assertEqual(len(merge(pieces)['cell_runs']), 35)

    def test_a_different_instrument_build_or_calibration_is_refused(self):
        for path, value in [(('rig', 'engine', 'commit'), 'other'), (('rig', 'mock', 'sha256'), 'other'),
                            (('build',), 'getbusbar/busbar:9.9.9'), (('rig', 'box_qualify', 'baseline_rps'), 1.0),
                            (('matrix', 'sweep_dur'), 99), (('definitions', 'memory'), 'changed')]:
            pieces = split(self.snap, self.units)
            target = pieces[3][1]
            for k in path[:-1]:
                target = target[k]
            target[path[-1]] = value
            with self.assertRaises(MergeError, msg=path):
                merge(pieces, full_grid())

    def test_an_unqualified_box_is_refused(self):
        pieces = split(self.snap, self.units)
        pieces[0][1]['rig']['box_qualify']['outcome'] = 'fail'
        with self.assertRaisesRegex(MergeError, 'qualif'):
            merge(pieces, full_grid())

    def test_a_per_box_memory_reading_is_kept_per_cell_not_chosen(self):
        pieces = split(self.snap, self.units)
        pieces[0][1]['matrix']['memory'] = {'idle_rss_mib': 10}
        merged = merge(pieces, full_grid())
        self.assertIsNone(merged['matrix']['memory'])
        self.assertIn('matrix.memory', merged['placement']['per_cell_fields'])
        self.assertEqual(merged['cell_runs'][[r['file'] for r in merged['cell_runs']].index(Path(pieces[0][0]).name)]['matrix_memory'], {'idle_rss_mib': 10})


if __name__ == '__main__':
    unittest.main()
