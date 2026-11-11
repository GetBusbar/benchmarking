#!/usr/bin/env python3
"""Compare a candidate-check snapshot (results/candidates/) with the published result for the same cells.

Side by side, per metric: the published value, the candidate value, the absolute and percent
difference. Beside them, both runs' instrument: engine commit, mock sha, calibration, qualification
reading and box id. The comparability verdict is MECHANICAL and nothing else: the same engine commit,
mock sha, calibration file, protocol pair and metric definition means comparable; otherwise it names
exactly what differs. No interpretation of the differences is made or implied.
"""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import sys

SECTIONS = ('perf', 'stream', 'memory')


def scalar(v):
    """A metric's recorded number: a bare number, or the `value` of an engine envelope."""
    if isinstance(v, bool):
        return None
    if isinstance(v, (int, float)):
        return v
    if isinstance(v, dict) and 'value' in v and isinstance(v['value'], (int, float)) and not isinstance(v['value'], bool):
        return v['value']
    return None


def is_metric(v):
    return scalar(v) is not None or (isinstance(v, dict) and 'value' in v) or v is None


def metrics(cell):
    """{metric name: raw recorded value} for every scalar metric of a cell (series are not metrics)."""
    out = {}
    if not isinstance(cell, dict):
        return out
    for section in SECTIONS:
        block = cell.get(section)
        if not isinstance(block, dict):
            continue
        for key, v in block.items():
            if key == 'frontier' and isinstance(v, list):
                for row in v:
                    if isinstance(row, dict) and 'p99_bound_us' in row:
                        out[f'{section}.frontier[p99<={row["p99_bound_us"]}us].rps'] = row.get('rps')
                continue
            if isinstance(v, (list, str, bool)):
                continue
            if is_metric(v):
                out[f'{section}.{key}'] = v
    return out


def definition(defs, name):
    """The engine's definitions governing a metric: every definitions entry of its section
    (`memory`, `perf.*`, `stream.*`). Compared as a block, so no metric is mapped to a definition by
    guesswork; any change to its section's definitions makes the metric not comparable."""
    section = name.partition('.')[0]
    block = {k: v for k, v in (defs or {}).items() if k == section or k.startswith(section + '.')}
    return section, block


def cell_of(snap, cell):
    ingress, egress = cell.split('>')
    return (((snap.get('matrix') or {}).get('upstreams') or {}).get(egress) or {}).get('cells', {}).get(ingress)


def calibration_for(root, snap):
    """The control-calibration file this run's qualification baseline came from, by the exact rule
    lib/qualification_baseline.py applies (engine + mock + instance type, median of the observations)."""
    rig = snap.get('rig') or {}
    engine = (rig.get('engine') or {}).get('commit')
    mock = (rig.get('mock') or {}).get('sha256')
    baseline = (rig.get('box_qualify') or {}).get('baseline_rps')
    if snap.get('candidate_check', {}).get('calibration'):
        return snap['candidate_check']['calibration']
    for path in sorted((Path(root) / 'results/calibrations').glob('*.json')):
        try:
            c = json.loads(path.read_text())
        except ValueError:
            continue
        if (c.get('engine_commit'), c.get('mock_sha256')) != (engine, mock):
            continue
        obs = [o.get('rps') for o in c.get('observations') or [] if isinstance(o.get('rps'), (int, float))]
        if obs and baseline is not None and statistics.median(obs) == baseline:
            return path.name
    return None


def instrument(root, snap, path):
    rig = snap.get('rig') or {}
    runs = snap.get('cell_runs') or []
    q = rig.get('box_qualify') or {}
    boxes = [r.get('box') for r in runs if r.get('box')] or ([snap['candidate_check']['box']] if snap.get('candidate_check', {}).get('box') else [])
    return {
        'file': Path(path).name,
        'sha256': hashlib.sha256(Path(path).read_bytes()).hexdigest(),
        'build': snap.get('build'),
        'measured_at': snap.get('measured_at'),
        'engine_commit': (rig.get('engine') or {}).get('commit'),
        'engine_dirty': (rig.get('engine') or {}).get('dirty'),
        'mock_sha256': (rig.get('mock') or {}).get('sha256'),
        'calibration': calibration_for(root, snap),
        'qualification': [r.get('rig', {}).get('box_qualify') for r in runs] or [q],
        'boxes': boxes or None,
        'placement': (snap.get('placement') or {}).get('mode', 'per-gateway'),
        'image_digest': (snap.get('candidate_check') or {}).get('image'),
    }


def compare(root, candidate, cand_path, published, pub_path, cells):
    a = instrument(root, published, pub_path)
    b = instrument(root, candidate, cand_path)
    shared = []
    for key, label in (('engine_commit', 'engine commit'), ('mock_sha256', 'mock sha256'), ('calibration', 'calibration file')):
        if a[key] is None or b[key] is None or a[key] != b[key]:
            shared.append(f'{label}: published {a[key]} vs candidate {b[key]}')
    rows, verdicts = [], {}
    for cell in cells:
        pc, cc = cell_of(published, cell), cell_of(candidate, cell)
        cell_diff = list(shared)
        if pc is None:
            cell_diff.append('protocol pair: not in the published result')
        if cc is None:
            cell_diff.append('protocol pair: not in the candidate result')
        pm, cm = metrics(pc), metrics(cc)
        for name in sorted(set(pm) | set(cm)):
            pv, cv = scalar(pm.get(name)), scalar(cm.get(name))
            dk_a, da = definition(published.get('definitions'), name)
            dk_b, db = definition(candidate.get('definitions'), name)
            reasons = list(cell_diff)
            if da != db:
                reasons.append(f'metric definition ({dk_a} section): differs between the two runs')
            row = {'cell': cell, 'metric': name, 'published': pv, 'candidate': cv,
                   'abs_diff': None if pv is None or cv is None else cv - pv,
                   'pct_diff': None if pv in (None, 0) or cv is None else (cv - pv) / abs(pv) * 100,
                   'published_raw': pm.get(name) if pv is None else None,
                   'candidate_raw': cm.get(name) if cv is None else None,
                   'comparable': not reasons, 'differs': reasons}
            rows.append(row)
        verdicts[cell] = {'comparable': not cell_diff, 'differs': cell_diff,
                          'published_served': (pc or {}).get('served'), 'candidate_served': (cc or {}).get('served')}
    return {'gateway': candidate.get('gateway'), 'cells': cells, 'published': a, 'candidate': b,
            'verdict': {'comparable': all(v['comparable'] for v in verdicts.values()) and all(r['comparable'] for r in rows),
                        'cells': verdicts,
                        'rule': 'comparable iff same engine commit, mock sha256, calibration file, protocol pair and metric definition'},
            'metrics': rows}


def fmt(v):
    if v is None:
        return 'n/a'
    if isinstance(v, float):
        return f'{v:.4g}'
    return str(v)


def markdown(report):
    a, b = report['published'], report['candidate']
    lines = [f"# Candidate check: {report['gateway']} {', '.join(report['cells'])}", '',
             f"Verdict: **{'comparable' if report['verdict']['comparable'] else 'not comparable'}** "
             f"({report['verdict']['rule']}).", '']
    for cell, v in report['verdict']['cells'].items():
        lines.append(f"- {cell}: {'comparable' if v['comparable'] else 'differs: ' + '; '.join(v['differs'])}"
                     f" (served: published {fmt(v['published_served'])}, candidate {fmt(v['candidate_served'])})")
    lines += ['', '| | published | candidate |', '|---|---|---|']
    for key in ('file', 'sha256', 'build', 'image_digest', 'measured_at', 'engine_commit', 'engine_dirty',
                'mock_sha256', 'calibration', 'placement'):
        lines.append(f'| {key} | `{fmt(a[key])}` | `{fmt(b[key])}` |')
    def quals(x):
        return '; '.join(f"{q.get('outcome')} {fmt(q.get('observed_rps'))} rps vs {fmt(q.get('baseline_rps'))} ({fmt(q.get('drift_pct'))}%)"
                         for q in x['qualification'] if q) or 'n/a'
    def boxes(x):
        return ', '.join(str((bx or {}).get('instance_id')) for bx in (x['boxes'] or [])) or 'not recorded'
    lines.append(f'| qualification | {quals(a)} | {quals(b)} |')
    lines.append(f'| box ids | {boxes(a)} | {boxes(b)} |')
    lines += ['', '| cell | metric | published | candidate | abs diff | % diff | comparable |', '|---|---|---|---|---|---|---|']
    for r in report['metrics']:
        lines.append(f"| {r['cell']} | {r['metric']} | {fmt(r['published'])} | {fmt(r['candidate'])} | "
                     f"{fmt(r['abs_diff'])} | {fmt(r['pct_diff'])} | {'yes' if r['comparable'] else 'no: ' + '; '.join(r['differs'])} |")
    return '\n'.join(lines) + '\n'


def published_for(root, gateway, cells):
    """The newest published snapshot of this gateway that carries every named cell and is not withdrawn."""
    withdrawn_path = Path(root) / 'results/withdrawn-runs.json'
    withdrawn = json.loads(withdrawn_path.read_text()).get('runs', {}) if withdrawn_path.exists() else {}
    for path in sorted((Path(root) / 'results/snapshots').glob(f'result_{gateway}_*.json'), reverse=True):
        raw = path.read_bytes()
        if hashlib.sha256(raw).hexdigest() in withdrawn:
            continue
        snap = json.loads(raw)
        if snap.get('candidate'):
            continue
        if all(cell_of(snap, c) is not None for c in cells):
            return path, snap
    raise SystemExit(f'No published {gateway} snapshot carries {", ".join(cells)}')


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('candidate', type=Path)
    p.add_argument('--published', type=Path, help='default: newest published snapshot carrying the cells')
    p.add_argument('--root', type=Path, default=Path(__file__).resolve().parent.parent)
    p.add_argument('--out', type=Path, required=True, help='report path stem; writes <stem>.md and <stem>.json')
    args = p.parse_args(argv)
    cand = json.loads(args.candidate.read_text())
    if cand.get('candidate') is not True:
        raise SystemExit(f'{args.candidate} is not stamped candidate: true')
    cells = (cand.get('candidate_check') or {}).get('cells') or sorted(
        f'{i}>{e}' for e, up in cand['matrix']['upstreams'].items() for i in up['cells'])
    if args.published:
        pub_path, pub = args.published, json.loads(args.published.read_text())
    else:
        pub_path, pub = published_for(args.root, cand['gateway'], cells)
    report = compare(args.root, cand, args.candidate, pub, pub_path, cells)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    Path(str(args.out) + '.json').write_text(json.dumps(report, indent=2) + '\n')
    Path(str(args.out) + '.md').write_text(markdown(report))
    print(f'{args.out}.md\n{args.out}.json')
    return 0


if __name__ == '__main__':
    sys.exit(main())
