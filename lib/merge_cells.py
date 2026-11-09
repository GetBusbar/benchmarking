#!/usr/bin/env python3
"""Merge the per-cell snapshots of ONE gateway (one fresh box per cell) into one result snapshot.

Faithful to the engine, never a re-reading of it:

  * MATRIX CELLS are copied verbatim from the box that measured them. Every grid coordinate must come
    from exactly one piece; a coordinate two pieces both carry is refused, never chosen between.
  * GATEWAY-LEVEL FIELDS are facts about the instrument, the build or the manifest, so every box must
    report the same value. A disagreement is refused, not resolved. Any field this tool does not name
    below is treated as gateway-level, so a field a future engine adds is checked, not dropped.
  * PER-CELL FIELDS are facts about one box or one process: kept per piece in `cell_runs`, and on the
    merged root only when every piece agrees (otherwise null, listed in `placement.per_cell_fields`).
  * DERIVED FIELDS follow the engine's own rule for the same field (engine/src/suite.rs):
    `matrix.served` and `upstreams.<egress>.served` are the OR over the cells (record_upstream_cell,
    any_served); `measured_at` is the newest piece's, the moment the grid was complete.

The rig's box qualification is per box: its gateway-level part (outcome, band, concurrency and the
calibration baseline, which must agree because every box judges against the same control calibration)
stays at rig.box_qualify; each box's own observation (observed_rps, drift_pct, attempts) lives in
cell_runs[].rig.box_qualify.
"""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import sys

DIALECTS = ['openai', 'openai-responses', 'anthropic', 'gemini', 'cohere', 'bedrock']

# Root fields that differ by box or by process; kept per piece.
ROOT_PER_CELL = {'measured_at', 'started_at', 'finished_at', 'duration_s', 'phase_s', 'memory', 'streaming'}
# Root fields this tool builds itself.
ROOT_DERIVED = {'rig', 'matrix', 'placement', 'cell_runs'}
# Matrix fields that differ by box, mirror the root, or are derived.
MATRIX_PER_CELL = {'memory'}
MATRIX_MIRRORED = {'rig', 'measured_at', 'started_at', 'finished_at', 'duration_s', 'phase_s'}
MATRIX_DERIVED = {'served', 'upstreams'}
# Upstream column fields: `served` is the OR over its cells; everything else is gateway-level.
UPSTREAM_DERIVED = {'served', 'cells'}
# rig fields that belong to one box's own observation.
QUALIFY_PER_BOX = {'observed_rps', 'drift_pct', 'observed_absent_reason', 'observed_absent_detail', 'attempts'}

METHOD_NOTE = ('Per-cell placement: every measured cell ran on its own fresh box, each qualified against '
               'the same control calibration. Box-to-box variance enters comparisons between cells of '
               'this gateway, and memory starts from a fresh process for every cell. Not the per-gateway '
               'method; do not mix the two in one comparison until an A/B run shows they agree.')


class MergeError(ValueError):
    pass


def _canon(v):
    return json.dumps(v, sort_keys=True)


def _agree(name, values):
    """The one value every piece reports, or refuse naming the field and the pieces that differ."""
    first = values[0][1]
    for label, v in values[1:]:
        if _canon(v) != _canon(first):
            raise MergeError(f'{name} differs between boxes: {values[0][0]}={_canon(first)[:160]} '
                             f'vs {label}={_canon(v)[:160]}')
    return copy.deepcopy(first)


def _ordered(keys, order):
    rank = {k: i for i, k in enumerate(order)}
    return sorted(keys, key=lambda k: (rank.get(k, len(order)), k))


def coords(snap):
    out = []
    for egress, up in ((snap.get('matrix') or {}).get('upstreams') or {}).items():
        for ingress in ((up or {}).get('cells') or {}):
            out.append(f'{ingress}>{egress}')
    return out


def merge(pieces, expect_cells=None):
    """pieces: [(label, snapshot dict, box dict or None, sha256 or None)]. Returns the merged snapshot."""
    if not pieces:
        raise MergeError('No per-cell snapshots to merge')
    snaps = [(p[0], p[1]) for p in pieces]
    gateway = _agree('gateway', [(l, s.get('gateway')) for l, s in snaps])
    for l, s in snaps:
        if not isinstance(s.get('matrix'), dict) or not isinstance(s['matrix'].get('upstreams'), dict):
            raise MergeError(f'{l} has no matrix.upstreams')
        if s['matrix'].get('cells') not in ({}, None):
            raise MergeError(f'{l} carries a v1 top-level matrix.cells row; this tool merges v2 grids only')

    # Every coordinate from exactly one box.
    owner = {}
    for l, s in snaps:
        for c in coords(s):
            if c in owner:
                raise MergeError(f'cell {c} is in both {owner[c]} and {l}; refusing to choose between two measurements')
            owner[c] = l
    if expect_cells is not None:
        missing = sorted(set(expect_cells) - set(owner))
        extra = sorted(set(owner) - set(expect_cells))
        if missing or extra:
            raise MergeError(f'merged grid is not the expected one: missing {missing or "none"}, unexpected {extra or "none"}')

    out = {}
    per_cell_fields = []
    root_keys = []
    for _, s in snaps:
        root_keys += [k for k in s if k not in root_keys]
    for key in root_keys:
        values = [(l, s.get(key)) for l, s in snaps]
        if key in ROOT_DERIVED:
            continue
        if key == 'measured_at':
            stamps = [v for _, v in values]
            if any(not isinstance(v, str) for v in stamps):
                raise MergeError('a piece has no measured_at')
            out[key] = max(stamps)              # ISO-8601 Z stamps order lexically
        elif key in ROOT_PER_CELL:
            if len({_canon(v) for _, v in values}) == 1:
                out[key] = copy.deepcopy(values[0][1])
            else:
                out[key] = None
                per_cell_fields.append(key)
        else:
            out[key] = _agree(key, values)

    # rig: instrument facts must agree; the box's own qualification reading is per cell.
    rigs = [(l, s.get('rig') or {}) for l, s in snaps]
    rig = {}
    rig_keys = []
    for _, r in rigs:
        rig_keys += [k for k in r if k not in rig_keys]
    for key in rig_keys:
        if key == 'box_qualify':
            continue
        rig[key] = _agree(f'rig.{key}', [(l, r.get(key)) for l, r in rigs])
    quals = [(l, r.get('box_qualify') or {}) for l, r in rigs]
    for l, q in quals:
        if q.get('outcome') not in ('pass', 'seed'):
            raise MergeError(f'{l}: box qualification outcome {q.get("outcome")!r}; only qualified boxes are merged')
    qual_keys = []
    for _, q in quals:
        qual_keys += [k for k in q if k not in qual_keys and k not in QUALIFY_PER_BOX]
    rig['box_qualify'] = {k: _agree(f'rig.box_qualify.{k}', [(l, q.get(k)) for l, q in quals]) for k in qual_keys}
    rig['box_qualify']['boxes'] = len(pieces)
    rig['box_qualify']['per_box'] = 'cell_runs[].rig.box_qualify'
    out['rig'] = rig

    # matrix
    mats = [(l, s['matrix']) for l, s in snaps]
    matrix = {}
    mat_keys = []
    for _, m in mats:
        mat_keys += [k for k in m if k not in mat_keys]
    upstreams = {}
    for key in mat_keys:
        values = [(l, m.get(key)) for l, m in mats]
        if key == 'served':
            matrix[key] = any(v is True for _, v in values)
        elif key == 'upstreams':
            matrix[key] = None              # filled below, keeps the engine's key position
        elif key in MATRIX_MIRRORED:
            matrix[key] = 'MIRROR'
        elif key in MATRIX_PER_CELL:
            if len({_canon(v) for _, v in values}) == 1:
                matrix[key] = copy.deepcopy(values[0][1])
            else:
                matrix[key] = None
                per_cell_fields.append(f'matrix.{key}')
        else:
            matrix[key] = _agree(f'matrix.{key}', values)
    egresses = []
    for _, m in mats:
        egresses += [e for e in m['upstreams'] if e not in egresses]
    for egress in _ordered(egresses, DIALECTS):
        cols = [(l, m['upstreams'][egress]) for l, m in mats if egress in m['upstreams']]
        col = {}
        col_keys = []
        for _, c in cols:
            col_keys += [k for k in c if k not in col_keys]
        for key in col_keys:
            if key == 'served':
                col[key] = any(c.get('served') is True for _, c in cols)
            elif key == 'cells':
                col[key] = {}
            else:
                col[key] = _agree(f'upstreams.{egress}.{key}', [(l, c.get(key)) for l, c in cols])
        cells = {}
        for _, c in cols:
            cells.update(c.get('cells') or {})
        col['cells'] = {i: copy.deepcopy(cells[i]) for i in _ordered(cells, DIALECTS)}
        upstreams[egress] = col
    matrix['upstreams'] = upstreams
    for key, v in list(matrix.items()):
        if v == 'MIRROR':
            matrix[key] = copy.deepcopy(out['rig']) if key == 'rig' else copy.deepcopy(out.get(key))
    out['matrix'] = matrix

    out['placement'] = {'mode': 'per-cell', 'boxes': len(pieces), 'per_cell_fields': per_cell_fields,
                        'note': METHOD_NOTE}
    runs = []
    for label, s, box, sha in pieces:
        runs.append({
            'cells': sorted(coords(s)),
            'file': Path(label).name,
            'sha256': sha,
            'box': box,
            'measured_at': s.get('measured_at'),
            'started_at': s.get('started_at'),
            'finished_at': s.get('finished_at'),
            'duration_s': s.get('duration_s'),
            'phase_s': s.get('phase_s'),
            'memory': s.get('memory'),
            'streaming': s.get('streaming'),
            'matrix_memory': s['matrix'].get('memory'),
            'rig': s.get('rig'),
        })
    out['cell_runs'] = sorted(runs, key=lambda r: r['cells'][0] if r['cells'] else '')
    return out


def load_piece(path):
    path = Path(path)
    raw = path.read_bytes()
    box_path = path.parent / 'box.json'
    box = json.loads(box_path.read_text()) if box_path.is_file() else None
    return (str(path), json.loads(raw), box, hashlib.sha256(raw).hexdigest())


def full_grid():
    return [f'{i}>{e}' for e in DIALECTS for i in DIALECTS]


def output_name(snap):
    return f"result_{snap['gateway']}_{snap['measured_at'].replace(':', '-')}.json"


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('pieces', nargs='+', type=Path, help='per-cell result_<gw>_*.json files (box.json beside each is read if present)')
    p.add_argument('--out-dir', type=Path, required=True)
    p.add_argument('--partial', action='store_true', help='allow a merged grid that is not the full 6x6')
    args = p.parse_args(argv)
    pieces = [load_piece(x) for x in args.pieces]
    try:
        merged = merge(pieces, None if args.partial else full_grid())
    except MergeError as exc:
        print(f'merge_cells: REFUSING: {exc}', file=sys.stderr)
        return 2
    args.out_dir.mkdir(parents=True, exist_ok=True)
    target = args.out_dir / output_name(merged)
    if target.exists():
        print(f'merge_cells: REFUSING to overwrite {target}', file=sys.stderr)
        return 2
    target.write_text(json.dumps(merged, indent=2) + '\n')
    print(target)
    return 0


if __name__ == '__main__':
    sys.exit(main())
