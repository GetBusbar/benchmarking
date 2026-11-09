import copy
import json
from pathlib import Path
import tempfile
import unittest

import candidate_report
from merge_cells import merge
from merge_cells_test import split, BUSBAR

ROOT = Path(__file__).resolve().parent.parent


class CandidateReport(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.published = json.loads(BUSBAR.read_text())
        cls.calibration = candidate_report.calibration_for(ROOT, cls.published)

    def candidate(self, **rig):
        snap = merge(split(self.published, [['openai>openai']]))
        cell = snap['matrix']['upstreams']['openai']['cells']['openai']
        cell['perf']['added_latency_p99_us'] = 110          # published 88
        for k, v in rig.items():
            snap['rig'][k] = v
        snap['candidate'] = True
        snap['candidate_check'] = {'cells': ['openai>openai'], 'calibration': self.calibration,
                                   'image': {'ref': 'getbusbar/busbar:1.6.0-rc1', 'id': 'sha256:abc'}}
        return snap

    def report(self, cand):
        with tempfile.TemporaryDirectory() as d:
            cp = Path(d) / 'cand.json'
            cp.write_text(json.dumps(cand))
            return candidate_report.compare(ROOT, cand, cp, self.published, BUSBAR, ['openai>openai']), cp

    def test_the_published_calibration_is_resolved_by_the_baseline_rule(self):
        self.assertEqual(self.calibration, '2026-11-09-c4965b2b-arm64.json')

    def test_same_instrument_is_comparable_and_differences_are_plain_arithmetic(self):
        report, _ = self.report(self.candidate())
        self.assertTrue(report['verdict']['comparable'], report['verdict'])
        row = next(r for r in report['metrics'] if r['metric'] == 'perf.added_latency_p99_us')
        self.assertEqual((row['published'], row['candidate'], row['abs_diff']), (88, 110, 22))
        self.assertAlmostEqual(row['pct_diff'], 25.0)
        same = next(r for r in report['metrics'] if r['metric'] == 'perf.added_latency_p50_us')
        self.assertEqual(same['abs_diff'], 0)
        self.assertTrue(any(r['metric'].startswith('perf.frontier[') for r in report['metrics']))
        self.assertTrue(any(r['metric'].startswith('memory.') for r in report['metrics']))
        self.assertEqual(report['candidate']['image_digest']['id'], 'sha256:abc')
        self.assertEqual(report['candidate']['boxes'][0]['instance_id'], 'i-00000000000000000')
        self.assertEqual(report['published']['qualification'][0]['observed_rps'], 94702.0)

    def test_a_different_engine_names_exactly_what_differs(self):
        engine = copy.deepcopy(self.published['rig']['engine'])
        engine['commit'] = 'fe27cc5aeae4479ac61efc577014f8cb2c9a759b'
        report, _ = self.report(self.candidate(engine=engine))
        self.assertFalse(report['verdict']['comparable'])
        why = report['verdict']['cells']['openai>openai']['differs']
        self.assertEqual(len(why), 1)
        self.assertIn('engine commit: published c4965b2b', why[0])

    def test_a_changed_definition_makes_only_its_section_incomparable(self):
        cand = self.candidate()
        cand['definitions'] = dict(cand['definitions'], memory='changed')
        report, _ = self.report(cand)
        rows = {r['metric']: r for r in report['metrics']}
        self.assertFalse(rows['memory.idle_rss_mib']['comparable'])
        self.assertTrue(rows['perf.added_latency_p99_us']['comparable'])

    def test_markdown_and_json_are_written(self):
        cand = self.candidate()
        with tempfile.TemporaryDirectory() as d:
            cp = Path(d) / 'cand.json'
            cp.write_text(json.dumps(cand))
            candidate_report.main([str(cp), '--published', str(BUSBAR), '--out', str(Path(d) / 'report')])
            md = (Path(d) / 'report.md').read_text()
            self.assertIn('Verdict: **comparable**', md)
            self.assertIn('| openai>openai | perf.added_latency_p99_us | 88 | 110 | 22 | 25 | yes |', md)
            self.assertTrue(json.loads((Path(d) / 'report.json').read_text())['verdict']['comparable'])

    def test_an_unstamped_snapshot_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            cp = Path(d) / 'x.json'
            cp.write_text(json.dumps({'gateway': 'busbar'}))
            with self.assertRaises(SystemExit):
                candidate_report.main([str(cp), '--out', str(Path(d) / 'r')])


if __name__ == '__main__':
    unittest.main()
