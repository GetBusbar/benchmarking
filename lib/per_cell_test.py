import importlib.util
from pathlib import Path
import unittest

import box_band
import per_cell

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('run_field', ROOT / 'run-field.py')
run_field = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run_field)


def units(gateway, n, est=600):
    return [{'gateway': gateway, 'cells': [f'c{i}'], 'est_s': est + i} for i in range(n)]


class Units(unittest.TestCase):
    def test_declared_zero_and_untestable_cells_ride_on_the_first_box(self):
        manifest = {'matrix': ['110000'] + ['000000'] * 5, 'untestable': ['anthropic/openai']}
        measured, unsupported = per_cell.classify(manifest)
        self.assertEqual(measured, ['openai>openai', 'openai>openai-responses'])
        self.assertEqual(len(unsupported), 34)
        got = per_cell.units_for('gw', manifest)
        self.assertEqual(len(got), 2)
        self.assertEqual(got[0]['cells'][0], 'openai>openai')
        self.assertEqual(len(got[0]['cells']), 35)
        self.assertEqual(got[1]['cells'], ['openai>openai-responses'])

    def test_undeclared_matrix_probes_every_cell(self):
        self.assertEqual(len(per_cell.units_for('gw', {})), 36)

    def test_a_candidate_names_measurable_cells_only(self):
        manifest = {'matrix': ['100000'] + ['000000'] * 5}
        self.assertEqual(per_cell.units_for('gw', manifest, ['openai>openai']), [{'gateway': 'gw', 'cells': ['openai>openai']}])
        with self.assertRaisesRegex(ValueError, 'unsupported'):
            per_cell.units_for('gw', manifest, ['cohere>openai'])
        with self.assertRaisesRegex(ValueError, 'not cells'):
            per_cell.units_for('gw', manifest, ['openai>vertex'])

    def test_every_current_gateway_expands(self):
        for d in sorted((ROOT / 'gateways').glob('*/definition.json')):
            got = per_cell.units_for(d.parent.name, per_cell.load_manifest(ROOT, d.parent.name))
            self.assertEqual(sum(len(u['cells']) for u in got), 36, d.parent.name)


class Schedule(unittest.TestCase):
    def test_whole_gateways_are_packed_and_large_ones_split(self):
        batches = per_cell.schedule(units('a', 5) + units('b', 10) + units('c', 20), 16)
        self.assertEqual([len(b) for b in batches], [16, 15, 4])
        self.assertEqual({u['gateway'] for u in batches[0]}, {'c'})
        self.assertEqual({u['gateway'] for u in batches[1]}, {'a', 'b'})
        # Longest-expected cells of a split gateway go first.
        self.assertEqual(batches[0][0]['est_s'], 619)
        self.assertEqual(sorted(u['est_s'] for u in batches[2]), [600, 601, 602, 603])

    def test_small_gateways_fill_a_batch_instead_of_leaving_it_half_empty(self):
        batches = per_cell.schedule(units('a', 12) + units('b', 13) + units('c', 2) + units('d', 1), 14)
        self.assertEqual([len(b) for b in batches], [14, 14])
        for g in 'abcd':
            self.assertEqual(len({i for i, b in enumerate(batches) for u in b if u['gateway'] == g}), 1, g)

    def test_summary_counts_every_box_once(self):
        s = per_cell.plan_summary(per_cell.schedule(units('a', 3, 600), 2), 0.5, overhead_s=600)
        self.assertEqual(s['boxes'], 3)
        self.assertAlmostEqual(s['wall_s'], (602 + 600) + (600 + 600))
        self.assertAlmostEqual(s['box_hours'], (600 + 601 + 602 + 3 * 600) / 3600)
        self.assertAlmostEqual(s['cost_usd'], s['box_hours'] * 0.5)

    def test_no_box_is_an_error_not_an_empty_field(self):
        with self.assertRaises(ValueError):
            per_cell.schedule(units('a', 1), 0)

    def test_quota_budget_keeps_the_margin(self):
        self.assertEqual(run_field.box_budget(256, 0, 32), 14)
        self.assertEqual(run_field.box_budget(1280, 0, 32), 78)
        self.assertEqual(run_field.box_budget(256, 240, 32), 0)
        self.assertEqual(run_field.box_budget(1280, 0, 32, cap=10), 10)


class Band(unittest.TestCase):
    def snap(self, drift, outcome='pass'):
        return {'rig': {'box_qualify': {'outcome': outcome, 'drift_pct': drift, 'observed_rps': 1, 'baseline_rps': 1}}}

    def test_one_sided_tighter_band(self):
        self.assertIsNone(box_band.check(self.snap(-1.9), 2))
        self.assertIsNone(box_band.check(self.snap(+9.0), 2))
        self.assertIn('outside', box_band.check(self.snap(-2.1), 2))

    def test_absent_reading_or_engine_fail_is_refused(self):
        self.assertIn('no drift', box_band.check(self.snap(None, 'seed'), 2))
        self.assertIn('outcome', box_band.check(self.snap(-1, 'fail'), 2))


if __name__ == '__main__':
    unittest.main()
